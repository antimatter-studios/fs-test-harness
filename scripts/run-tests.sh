#!/usr/bin/env bash
# run-tests.sh -- single entrypoint for the windows-VM test matrix.
#
# Wraps every step of the pipeline (bootstrap, preflight, ship, run,
# diag-pull) so that a single command does everything and cleans up
# after itself. No separate setup script to remember.
#
# Usage:
#   bash <harness>/scripts/run-tests.sh
#     Run the full matrix. On first run, prompts for VM details
#     (user, IP, ssh key, workdir, image dir), probes SSH, writes
#     ${CONSUMER_ROOT}/.test-env, then proceeds. Subsequent runs
#     skip the prompts.
#
#   bash <harness>/scripts/run-tests.sh SCENARIO
#     Substring filter against the matrix. Examples:
#       run-tests.sh basic-ro-list      # one explicit scenario
#       run-tests.sh basic-rw           # all basic-rw-* scenarios
#       run-tests.sh xattr              # all xattr-* scenarios
#
#   bash <harness>/scripts/run-tests.sh [SCENARIO] --build
#     Rebuild the consumer binary on the host first, before shipping.
#     Requires `[run].build_command` in harness.toml; otherwise no-op.
#
#   bash <harness>/scripts/run-tests.sh [SCENARIO] --ship
#     tar+ssh the consumer source tree to the VM first. Always-on for
#     now (the v1 model re-ships every run); the flag is reserved for
#     when v2 recipe mode lets us skip the ship.
#
#   bash <harness>/scripts/run-tests.sh --list [PATTERN]
#     List matrix scenarios matching the optional pattern; don't run.
#
#   bash <harness>/scripts/run-tests.sh --reset
#     Wipe ${CONSUMER_ROOT}/.test-env and re-prompt. Use after VM IP
#     change, ssh key rotation, etc.
#
#   bash <harness>/scripts/run-tests.sh --vm-host=USER@HOST [...]
#     Update one or more `.test-env` fields and run. Any value passed
#     this way is persisted to .test-env (no `--save` needed — the only
#     reason to type it is because you want to keep it). All fields:
#       --vm-host=USER@HOST
#       --ssh-key=PATH                (writes "-i KEY -o IdentitiesOnly=yes")
#       --vm-workdir=PATH             (default: harness.toml [vm].workdir)
#       --vm-image-dir=PATH           (default: harness.toml [vm].image_dir)
#
#   bash <harness>/scripts/run-tests.sh --help
#     This text. (Built from the leading comment block of this file —
#     so what you read here matches what the script actually does.)
#
# Bootstrap behaviour:
#   - .test-env present                      -> straight to preflight + run
#   - .test-env missing AND interactive TTY  -> prompt, write, continue
#   - .test-env missing AND --vm-host=...    -> use flags + write .test-env
#   - .test-env missing AND non-interactive  -> error with copy-paste hint
# Any --vm-*/--ssh-* flag updates .test-env (changes always persist).
#
# Output:
#   stdout: per-step push/run/pull lines plus the libtest-mimic per-
#           scenario summary at the end
#   diag:   ${CONSUMER_ROOT}/test-diagnostics/run-<UTC>/  (host)
#           <vm.workdir>/test-diagnostics/matrix/         (VM)
#
# Exit code:
#   0 if every matching scenario passed (or was ignored)
#   non-zero if at least one failed

set -euo pipefail

# shellcheck source=_lib_harness.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_lib_harness.sh"

echo "[harness] fs-test-harness $(harness_self_version)"

# ── arg parsing ─────────────────────────────────────────────────
DO_BUILD=0
DO_LIST=0
DO_RESET=0
SCENARIO=""
# Per-run / bootstrap overrides. Empty = unset; resolved later from
# .test-env or harness.toml or interactive prompts.
ARG_VM_HOST=""
ARG_VM_WORKDIR=""
ARG_VM_IMAGE_DIR=""
ARG_SSH_KEY=""

usage() {
    # The leading comment block is the documentation, so --help just
    # prints it. Stops at the first non-comment line.
    awk '
        NR == 1 { next }                       # skip shebang
        /^#/ { sub(/^# ?/, ""); print; next }  # strip "# " prefix
        { exit }                               # stop at first non-comment
    ' "${BASH_SOURCE[0]}"
}

for arg in "$@"; do
    case "$arg" in
        --build)            DO_BUILD=1 ;;
        --ship)             ;;  # reserved for future v2 mode; v1 always ships
        --list)             DO_LIST=1 ;;
        --reset)            DO_RESET=1 ;;
        --vm-host=*)        ARG_VM_HOST="${arg#*=}" ;;
        --vm-workdir=*)     ARG_VM_WORKDIR="${arg#*=}" ;;
        --vm-image-dir=*)   ARG_VM_IMAGE_DIR="${arg#*=}" ;;
        --ssh-key=*)        ARG_SSH_KEY="${arg#*=}" ;;
        --help|-h)          usage; exit 0 ;;
        --*)
            echo "[run-tests] unknown flag: ${arg}" >&2
            echo "[run-tests] use --help for usage" >&2
            exit 2
            ;;
        *)
            if [[ -n "${SCENARIO}" ]]; then
                echo "[run-tests] multiple positional args: '${SCENARIO}' and '${arg}'" >&2
                echo "[run-tests] only one scenario filter is supported" >&2
                exit 2
            fi
            SCENARIO="${arg}"
            ;;
    esac
done

ENV_FILE="${consumer_root}/.test-env"
PROJECT_NAME="$(harness_get_or project.name "consumer")"

# ── --reset: wipe .test-env, fall through to bootstrap ──────────
if [[ "${DO_RESET}" == "1" && -f "${ENV_FILE}" ]]; then
    echo "[run-tests] --reset: removing ${ENV_FILE}"
    rm -f "${ENV_FILE}"
fi

# ── --list: offline; just walk test-matrix.json ─────────────────
if [[ "${DO_LIST}" == "1" ]]; then
    matrix_path="$(harness_get_or project.matrix_path "test-matrix.json")"
    matrix_full="${consumer_root}/${matrix_path}"
    if [[ ! -f "${matrix_full}" ]]; then
        echo "[run-tests] matrix not found: ${matrix_full}" >&2
        exit 2
    fi
    python3 - "${matrix_full}" "${SCENARIO}" <<'PYEOF'
import json, sys
matrix_path, pat = sys.argv[1], sys.argv[2]
m = json.load(open(matrix_path))
scenarios = m.get("scenarios", {})
for name in sorted(scenarios):
    s = scenarios[name]
    if not isinstance(s, dict):
        continue
    if pat and pat not in name:
        continue
    shape = "v2" if s.get("recipe") else ("v1" if s.get("ops") else "??")
    status = s.get("status", "")
    print(f"  [{shape}] {name:<55} {status}")
PYEOF
    exit 0
fi

# ── detect run mode based on what the SCENARIO filter matches ───
# v1 scenarios use `ops:` and need a Windows orchestrator (the runner
# spawns `run-scenario.ps1` which only exists on Windows). The legacy
# path tar+ssh's source to the VM and runs `cargo run --bin run-matrix`
# there.
#
# v2 scenarios use `recipe:` and run through `dispatch::run_recipe`,
# which only needs a POSIX orchestrator with `ssh` in $PATH. The new
# path runs `cargo run --bin run-matrix` LOCALLY on the orchestrator
# host (e.g. Mac); per-step host=vm work tunnels through SSH on demand.
#
# Routing is per-invocation, picked from the FILTERED scenario set:
# if the filter matches any scenario with `recipe`, the whole run uses
# v2 mode. Otherwise v1 mode. Mixed-shape matrices migrate one
# scenario at a time; users invoke each shape with the appropriate
# substring filter during the migration window.
RUN_MODE="v1"
matrix_path_for_detect="$(harness_get_or project.matrix_path "test-matrix.json")"
matrix_full_for_detect="${consumer_root}/${matrix_path_for_detect}"
if [[ -f "${matrix_full_for_detect}" ]]; then
    RUN_MODE=$(python3 - "${matrix_full_for_detect}" "${SCENARIO}" <<'PYEOF'
import json, sys
try:
    m = json.load(open(sys.argv[1]))
except Exception:
    print("v1"); sys.exit(0)
pat = sys.argv[2]
saw_v1 = saw_v2 = False
for name, s in m.get("scenarios", {}).items():
    if not isinstance(s, dict): continue
    if pat and pat not in name: continue
    if s.get("recipe"):
        saw_v2 = True
    elif s.get("ops"):
        saw_v1 = True
# Prefer v2 when any v2 is matched; pure-v1 falls back to v1.
print("v2" if saw_v2 else "v1")
PYEOF
)
fi
echo "[run-tests] mode: ${RUN_MODE}${SCENARIO:+ (filter=${SCENARIO})}"

# ── v2 mode: cargo run locally; no ship-and-run-on-VM ───────────
# Pure host-side recipes don't need .test-env / SSH at all. (Once we
# add v2 scenarios with `host=vm` steps, this branch will need to fall
# through to bootstrap so SSH config is available for per-step
# tunnelling. For now every v2 step in this consumer is host-side.)
if [[ "${RUN_MODE}" == "v2" ]]; then
    if [[ "${DO_BUILD}" == "1" ]]; then
        BUILD_COMMAND="$(harness_get_or run.build_command "")"
        if [[ -z "${BUILD_COMMAND}" ]]; then
            echo "[run-tests] --build requested but [run].build_command not set in harness.toml; skipping" >&2
        else
            echo "[run-tests] === build phase ==="
            echo "[run-tests] ${BUILD_COMMAND}"
            ( cd "${consumer_root}" && eval "${BUILD_COMMAND}" )
        fi
    fi

    cd "${consumer_root}"
    EXTRA_ARGS=""
    [[ -n "${SCENARIO}" ]] && EXTRA_ARGS=$(printf ' %q' "${SCENARIO}")

    # Image-dir resolution priority for v2 mode (host-side):
    #   HARNESS_IMAGE_DIR env (caller override) >
    #   VM_IMAGE_DIR from .test-env >
    #   [run].image_dir from harness.toml (host-side path) >
    #   [vm].image_dir from harness.toml (v1 default; usually wrong on
    #     host because it points at the VM-relative path)
    # The [run] section lets a consumer point host-side ops at a
    # different physical dir than the v1-shipped [vm].image_dir
    # without breaking the v1 path. Common when test images live in a
    # sibling project that the consumer vendors as build artefacts.
    : "${HARNESS_IMAGE_DIR:=${VM_IMAGE_DIR:-$(harness_get_or run.image_dir "$(harness_get_or vm.image_dir '')")}}"
    export HARNESS_IMAGE_DIR
    export HARNESS_CONSUMER_ROOT="${consumer_root}"

    echo "[run]  cargo run --bin run-matrix locally (v2 mode)"
    echo "[run]  HARNESS_IMAGE_DIR=${HARNESS_IMAGE_DIR}"
    echo
    set +e
    cargo run --manifest-path "${harness_root}/runner/Cargo.toml" \
              --release --bin run-matrix -- --test-threads=1${EXTRA_ARGS}
    RUN_EXIT=$?
    set -e

    echo
    echo "==============================================================="
    echo "diagnostics: ${consumer_root}/test-diagnostics/matrix/"
    echo "test exit:   ${RUN_EXIT}  (0 = all passed/ignored; non-zero = at least one failed)"
    echo "==============================================================="
    exit ${RUN_EXIT}
fi

# ── bootstrap: ensure .test-env exists + populated ──────────────
# Three paths into a populated env:
#   (a) .test-env exists -> source it
#   (b) flags supplied   -> use them, write .test-env
#   (c) interactive TTY  -> prompt, write .test-env
# Anything else is a hard error with a copy-paste recovery hint.
prompt() {
    local var="$1" question="$2" default="${3:-}"
    local input
    if [[ -n "${default}" ]]; then
        read -r -p "${question} [${default}]: " input
        printf -v "${var}" '%s' "${input:-${default}}"
    else
        read -r -p "${question}: " input
        printf -v "${var}" '%s' "${input}"
    fi
}

bootstrap_interactive() {
    local default_host default_key default_workdir default_image_dir
    default_host="$(harness_get_or vm.host "")"
    default_key="$(harness_get_or vm.ssh_key "")"
    default_workdir="$(harness_get_or vm.workdir "")"
    default_image_dir="$(harness_get_or vm.image_dir "")"

    # Resolve relative SSH key paths against consumer_root.
    if [[ -n "${default_key}" && "${default_key}" != /* ]]; then
        default_key="$(cd "${consumer_root}" && cd "$(dirname "${default_key}")" 2>/dev/null && pwd)/$(basename "${default_key}")" \
            || default_key="${consumer_root}/${default_key}"
    fi

    echo "==============================================================="
    echo " ${PROJECT_NAME} -- first-run setup"
    echo "==============================================================="
    echo
    echo "No .test-env found at ${ENV_FILE}. Setting up now."
    echo "You'll need:"
    echo "  * The Windows VM's IP / hostname"
    echo "  * A user account with admin rights on the VM"
    echo "  * An SSH private key that account accepts (no password)"
    echo

    local vm_user vm_ip
    prompt vm_user "VM username"           "${default_host%%@*}"
    prompt vm_ip   "VM IP / hostname"      "${default_host##*@}"
    VM_HOST="${vm_user}@${vm_ip}"

    prompt SSH_KEY      "SSH private key (blank = use ssh-agent)" "${default_key}"
    prompt VM_WORKDIR   "Remote workdir on the VM"                "${default_workdir}"
    prompt VM_IMAGE_DIR "Remote dir holding test disk images"     "${default_image_dir}"
}

bootstrap_from_flags() {
    VM_HOST="${ARG_VM_HOST}"
    SSH_KEY="${ARG_SSH_KEY}"
    VM_WORKDIR="${ARG_VM_WORKDIR:-$(harness_get_or vm.workdir "")}"
    VM_IMAGE_DIR="${ARG_VM_IMAGE_DIR:-$(harness_get_or vm.image_dir "")}"
}

build_ssh_opts_from_key() {
    SSH_OPTS=""
    if [[ -n "${SSH_KEY:-}" ]]; then
        if [[ ! -f "${SSH_KEY}" ]]; then
            echo "[run-tests] ssh key not found at: ${SSH_KEY}" >&2
            exit 1
        fi
        SSH_OPTS="-i ${SSH_KEY} -o IdentitiesOnly=yes"
    fi
}

write_env_file() {
    cat > "${ENV_FILE}" <<EOF
# Generated by fs-test-harness/scripts/run-tests.sh on $(date '+%Y-%m-%d %H:%M:%S').
# Sourced automatically by run-tests.sh on subsequent runs.
# Re-run with --reset to regenerate.
export VM_HOST="${VM_HOST}"
export VM_WORKDIR="${VM_WORKDIR}"
export VM_IMAGE_DIR="${VM_IMAGE_DIR}"
export SSH_OPTS="${SSH_OPTS}"
EOF

    local gitignore="${consumer_root}/.gitignore"
    if [[ -f "${gitignore}" ]] && ! grep -qxF '.test-env' "${gitignore}"; then
        echo '.test-env' >> "${gitignore}"
        echo "[run-tests] added .test-env to .gitignore"
    fi
    echo "[run-tests] wrote ${ENV_FILE}"
}

if [[ -f "${ENV_FILE}" ]]; then
    # shellcheck disable=SC1090
    source "${ENV_FILE}"
elif [[ -n "${ARG_VM_HOST}" ]]; then
    bootstrap_from_flags
    build_ssh_opts_from_key
    write_env_file
elif [[ -t 0 && -t 1 ]]; then
    bootstrap_interactive
    build_ssh_opts_from_key
    write_env_file
else
    echo "[run-tests] ${ENV_FILE} missing and no flags supplied (non-interactive)." >&2
    echo "[run-tests] copy-paste to set up:" >&2
    echo "  bash $(basename "${BASH_SOURCE[0]}") \\" >&2
    echo "      --vm-host=USER@IP \\" >&2
    echo "      --ssh-key=/path/to/key \\" >&2
    echo "      --vm-workdir='C:/Users/USER/dev/${PROJECT_NAME}' \\" >&2
    echo "      --vm-image-dir=PATH" >&2
    exit 2
fi

# ── apply per-run overrides on top of .test-env ─────────────────
[[ -n "${ARG_VM_HOST}"      ]] && VM_HOST="${ARG_VM_HOST}"
[[ -n "${ARG_VM_WORKDIR}"   ]] && VM_WORKDIR="${ARG_VM_WORKDIR}"
[[ -n "${ARG_VM_IMAGE_DIR}" ]] && VM_IMAGE_DIR="${ARG_VM_IMAGE_DIR}"
if [[ -n "${ARG_SSH_KEY}" ]]; then
    SSH_KEY="${ARG_SSH_KEY}"
    build_ssh_opts_from_key
fi

# Persist any overrides. Reaching for one of these flags means the
# user wants the new value to stick — no `--save` opt-in.
if [[ -f "${ENV_FILE}" \
      && ( -n "${ARG_VM_HOST}" || -n "${ARG_VM_WORKDIR}" \
           || -n "${ARG_VM_IMAGE_DIR}" || -n "${ARG_SSH_KEY}" ) ]]; then
    write_env_file
fi

if [[ -z "${VM_HOST:-}" ]]; then
    echo "[run-tests] VM_HOST not set after bootstrap (this is a bug)" >&2
    exit 2
fi

# ── fill remaining defaults from harness.toml ───────────────────
VM_WORKDIR="${VM_WORKDIR:-$(harness_get_or vm.workdir "C:/Users/${VM_HOST%%@*}/dev/${PROJECT_NAME}")}"
VM_IMAGE_DIR="${VM_IMAGE_DIR:-$(harness_get_or vm.image_dir "")}"
SSH_OPTS="${SSH_OPTS:-}"
SSH_OPTS="${SSH_OPTS} -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=4"
VM_WORKDIR_PS="${VM_WORKDIR//\//\\}"

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
DIAG_BASE="${DIAG_DIR:-${consumer_root}/test-diagnostics}"
DIAG_LOCAL="${DIAG_BASE}/run-${TIMESTAMP}"
mkdir -p "${DIAG_LOCAL}"

cd "${consumer_root}"

# ── preflight: SSH reachability with actionable hints ───────────
# Lifted from rust-fs-ntfs/scripts/v2/test. The harness's per-op SSH
# calls otherwise fail with cryptic "exit 255" / one-liner stderr
# minutes into a run; we'd rather catch it in 5s with a hint.
preflight_ssh() {
    local probe_out probe_rc
    # shellcheck disable=SC2086
    probe_out=$(ssh ${SSH_OPTS} -o BatchMode=yes -o ConnectTimeout=5 \
        "${VM_HOST}" 'echo OK' 2>&1)
    probe_rc=$?
    if [[ "${probe_rc}" -eq 0 && "${probe_out}" == *OK* ]]; then
        return 0
    fi
    echo "[run-tests] preflight: SSH to ${VM_HOST} failed (rc=${probe_rc})" >&2
    echo "[run-tests] preflight: ${probe_out}" >&2

    # Auto-recover: missing known_hosts entry. The VM is already trusted
    # via .test-env; a fresh ssh-keyscan + retry is acceptable TOFU for
    # a dev VM the user explicitly pointed the wrapper at.
    if [[ "${probe_out}" == *"Host key verification failed"* \
       || "${probe_out}" == *"No matching host key"* ]]; then
        local ip="${VM_HOST#*@}"
        echo "[run-tests] preflight: auto-trusting ${ip}'s host key (ssh-keyscan -> ~/.ssh/known_hosts)" >&2
        if ssh-keyscan -H -T 5 "${ip}" >> "${HOME}/.ssh/known_hosts" 2>/dev/null; then
            # shellcheck disable=SC2086
            probe_out=$(ssh ${SSH_OPTS} -o BatchMode=yes -o ConnectTimeout=5 \
                "${VM_HOST}" 'echo OK' 2>&1)
            probe_rc=$?
            if [[ "${probe_rc}" -eq 0 && "${probe_out}" == *OK* ]]; then
                echo "[run-tests] preflight: SSH OK after keyscan" >&2
                return 0
            fi
        fi
        echo "[run-tests] preflight: ssh-keyscan didn't unblock — see above" >&2
    fi

    case "${probe_out}" in
        *"Connection refused"*)
            echo "[run-tests] hint: VM is up but sshd isn't listening on port 22 — check 'Get-Service sshd' on the VM" >&2 ;;
        *"Operation timed out"*|*"Network is unreachable"*|*"Host is down"*)
            echo "[run-tests] hint: can't reach ${VM_HOST#*@} — VM down? wrong IP? check 'ipconfig' on the VM and re-run with --reset" >&2 ;;
        *"Permission denied"*)
            echo "[run-tests] hint: ssh key rejected — check ssh_key path + the VM's ~/.ssh/authorized_keys, or re-run with --reset" >&2 ;;
    esac
    return 1
}

echo "[run-tests] preflight: SSH ${VM_HOST}"
if ! preflight_ssh; then
    echo "[run-tests] preflight failed; aborting before ship/run" >&2
    exit 2
fi

# ── optional build phase ────────────────────────────────────────
# `[run].build_command` in harness.toml lets each consumer declare its
# own build (e.g. "cargo build --release --bin ext4"). Empty = no-op.
BUILD_COMMAND="$(harness_get_or run.build_command "")"
if [[ "${DO_BUILD}" == "1" ]]; then
    if [[ -z "${BUILD_COMMAND}" ]]; then
        echo "[run-tests] --build requested but [run].build_command not set in harness.toml; skipping" >&2
    else
        echo "[run-tests] === build phase ==="
        echo "[run-tests] ${BUILD_COMMAND}"
        eval "${BUILD_COMMAND}"
    fi
fi

# ── ship phase: tar consumer source -> VM ───────────────────────
echo "[push] tar-ssh source -> ${VM_HOST}:${VM_WORKDIR}"
# shellcheck disable=SC2029,SC2086
ssh ${SSH_OPTS} "${VM_HOST}" "if (-not (Test-Path '${VM_WORKDIR_PS}')) { New-Item -ItemType Directory -Path '${VM_WORKDIR_PS}' -Force | Out-Null }"

TAR_EXCLUDES=(
    --exclude='./target' --exclude='./.git' --exclude='./.history'
    --exclude='./test-diagnostics' --exclude='./diag'
    --exclude='*.swp' --exclude='.DS_Store'
    --exclude='./privatekey' --exclude='./privatekey.*'
    --exclude='./.test-env'
)
if [[ -n "${HARNESS_TAR_EXCLUDE:-}" ]]; then
    # shellcheck disable=SC2206
    EXTRA_EX=( ${HARNESS_TAR_EXCLUDE} )
    TAR_EXCLUDES+=( "${EXTRA_EX[@]}" )
fi

# shellcheck disable=SC2086
tar "${TAR_EXCLUDES[@]}" -cf - . | \
    ssh ${SSH_OPTS} "${VM_HOST}" "tar -xf - -C '${VM_WORKDIR}'"

# Push the harness checkout itself if it lives outside the consumer tree
# (out-of-tree checkout). When vendored as a submodule under consumer
# tree, the source tar above already shipped it.
HARNESS_REL=""
case "${harness_root}" in
    "${consumer_root}"/*)
        HARNESS_REL="${harness_root#"${consumer_root}/"}"
        ;;
    *)
        HARNESS_REL="harness"
        echo "[push] harness -> ${VM_HOST}:${VM_WORKDIR}/${HARNESS_REL}"
        # shellcheck disable=SC2029,SC2086
        ssh ${SSH_OPTS} "${VM_HOST}" "if (-not (Test-Path '${VM_WORKDIR_PS}\\${HARNESS_REL}')) { New-Item -ItemType Directory -Path '${VM_WORKDIR_PS}\\${HARNESS_REL}' -Force | Out-Null }"
        # shellcheck disable=SC2086
        tar --exclude='./target' --exclude='./.git' \
            -C "${harness_root}" -cf - . | \
            ssh ${SSH_OPTS} "${VM_HOST}" "tar -xf - -C '${VM_WORKDIR}/${HARNESS_REL}'"
        ;;
esac

# Push the image_dir if it's outside the consumer tree.
if [[ -n "${VM_IMAGE_DIR}" ]]; then
    case "${VM_IMAGE_DIR}" in
        /*|[A-Za-z]:[/\\]*) ;;  # absolute paths handled by the consumer
        *)
            local_image_dir="${consumer_root}/${VM_IMAGE_DIR}"
            if [[ -d "${local_image_dir}" ]]; then
                resolved_image_dir="$(cd "${local_image_dir}" 2>/dev/null && pwd -P || echo "")"
                resolved_consumer="$(cd "${consumer_root}" && pwd -P)"
                case "${resolved_image_dir}" in
                    "${resolved_consumer}"|"${resolved_consumer}"/*) ;;
                    "")
                        echo "[push] image_dir ${VM_IMAGE_DIR} did not resolve; skipping" >&2
                        ;;
                    *)
                        remote_image_dir="${VM_WORKDIR}/${VM_IMAGE_DIR}"
                        for _ in 1 2 3; do
                            remote_image_dir="$(echo "${remote_image_dir}" | sed 's:/[^/]*/\.\./:/:g')"
                        done
                        remote_image_dir_ps="${remote_image_dir//\//\\}"
                        echo "[push] images -> ${VM_HOST}:${remote_image_dir}"
                        # shellcheck disable=SC2029,SC2086
                        ssh ${SSH_OPTS} "${VM_HOST}" "if (-not (Test-Path '${remote_image_dir_ps}')) { New-Item -ItemType Directory -Path '${remote_image_dir_ps}' -Force | Out-Null }"
                        # shellcheck disable=SC2086
                        tar -C "${resolved_image_dir}" --exclude='./.DS_Store' -cf - . | \
                            ssh ${SSH_OPTS} "${VM_HOST}" "tar -xf - -C '${remote_image_dir}'"
                        ;;
                esac
            else
                echo "[push] image_dir ${local_image_dir} not found locally; skipping" >&2
            fi
            ;;
    esac
fi

# ── run phase: cargo run --bin run-matrix on the VM ─────────────
EXTRA_ARGS=""
if [[ -n "${SCENARIO}" ]]; then
    EXTRA_ARGS=$(printf ' %q' "${SCENARIO}")
fi

IMAGE_DIR_ESCAPED="${VM_IMAGE_DIR//\\/\\\\}"
ENV_PREFIX="$(harness_get_or vm.env_prefix "")"

REMOTE_CMD="Set-Location '${VM_WORKDIR_PS}'; \$env:PATH=\"\$env:USERPROFILE\\.cargo\\bin;\$env:PATH\"; \$env:HARNESS_IMAGE_DIR='${IMAGE_DIR_ESCAPED}'; \$env:HARNESS_CONSUMER_ROOT='${VM_WORKDIR_PS}'; ${ENV_PREFIX} cargo run --manifest-path ${HARNESS_REL}/runner/Cargo.toml --release --bin run-matrix -- --test-threads=1${EXTRA_ARGS}"

echo "[run]  cargo run --bin run-matrix on ${VM_HOST}"
echo "[run]  remote: cargo run --bin run-matrix -- --test-threads=1${EXTRA_ARGS}"
echo
set +e
# shellcheck disable=SC2029,SC2086
ssh ${SSH_OPTS} "${VM_HOST}" "${REMOTE_CMD}"
RUN_EXIT=$?
set -e

# ── pull diagnostics back ───────────────────────────────────────
echo
echo "[pull] test-diagnostics -> ${DIAG_LOCAL}"
# shellcheck disable=SC2029,SC2086
ssh ${SSH_OPTS} "${VM_HOST}" "Set-Location '${VM_WORKDIR_PS}'; if (Test-Path 'test-diagnostics\\matrix') { tar -cf - test-diagnostics/matrix } else { exit 0 }" | \
    tar -xf - -C "${DIAG_LOCAL}" --strip-components=2 2>/dev/null || \
    echo "[pull] no test-diagnostics/matrix on remote (build may have failed before any scenario ran)"

echo
echo "==============================================================="
if [[ -f "${DIAG_LOCAL}/results.json" ]]; then
    PASS=$(grep -c '"status": "passed"' "${DIAG_LOCAL}/results.json" || true)
    FAIL=$(grep -c '"status": "failed"' "${DIAG_LOCAL}/results.json" || true)
    ERR=$(grep -c '"status": "errored"' "${DIAG_LOCAL}/results.json" || true)
    echo "results: ${PASS} passed, ${FAIL} failed, ${ERR} errored"
fi
echo "diagnostics: ${DIAG_LOCAL}"
echo "test exit:   ${RUN_EXIT}  (0 = all passed/ignored; non-zero = at least one failed)"
echo "==============================================================="

exit ${RUN_EXIT}
