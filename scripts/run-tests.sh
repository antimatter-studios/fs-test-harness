#!/usr/bin/env bash
# run-tests.sh -- single entrypoint for the windows-VM test matrix.
#
# Wraps every step of the pipeline (bootstrap, preflight, build, run,
# diag) so that a single command does everything. No separate setup
# script to remember.
#
# Since fs-test-harness 3.0.0 the runner only speaks v2 (recipe-based
# scenarios). This script always dispatches `cargo run --bin run-matrix`
# **locally on the orchestrator** (Mac / Linux / WSL2). The runner
# tunnels per-step SSH to the VM for any recipe step with `host: vm`.
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
#       run-tests.sh basic-ro-list     # one explicit scenario
#       run-tests.sh basic-rw          # all basic-rw-* scenarios
#       run-tests.sh xattr             # all xattr-* scenarios
#
#   bash <harness>/scripts/run-tests.sh [SCENARIO] --build
#     Rebuild the consumer binary on the host first. Requires
#     `[run].build_command` in harness.toml; otherwise no-op.
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
#     this way is persisted to .test-env (no `--save` needed — the
#     only reason to type it is because you want to keep it). All fields:
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
# Preflight: SSH reachability is probed only when the filtered scenario
# set contains at least one vm-step recipe. Pure host-side runs skip
# the probe — they don't need the VM.
#
# Output:
#   stdout: per-scenario PASS/FAIL from libtest-mimic + per-step diag
#           when failures occur
#   diag:   ${CONSUMER_ROOT}/test-diagnostics/matrix/<scenario>/
#           (overwritten per run; tar+inspect for archive)
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
ARG_VM_HOST=""
ARG_VM_WORKDIR=""
ARG_VM_IMAGE_DIR=""
ARG_SSH_KEY=""

usage() {
    awk '
        NR == 1 { next }                       # skip shebang
        /^#/ { sub(/^# ?/, ""); print; next }  # strip "# " prefix
        { exit }                               # stop at first non-comment
    ' "${BASH_SOURCE[0]}"
}

for arg in "$@"; do
    case "$arg" in
        --build)            DO_BUILD=1 ;;
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
matrix_path="$(harness_get_or project.matrix_path "test-matrix.json")"
matrix_full="${consumer_root}/${matrix_path}"

# ── --reset: wipe .test-env, fall through to bootstrap ──────────
if [[ "${DO_RESET}" == "1" && -f "${ENV_FILE}" ]]; then
    echo "[run-tests] --reset: removing ${ENV_FILE}"
    rm -f "${ENV_FILE}"
fi

# ── --list: offline; just walk test-matrix.json ─────────────────
if [[ "${DO_LIST}" == "1" ]]; then
    if [[ ! -f "${matrix_full}" ]]; then
        echo "[run-tests] matrix not found: ${matrix_full}" >&2
        exit 2
    fi
    python3 - "${matrix_full}" "${SCENARIO}" <<'PYEOF'
import json, sys
matrix_path, pat = sys.argv[1], sys.argv[2]
m = json.load(open(matrix_path))
for name in sorted(m.get("scenarios", {})):
    s = m["scenarios"][name]
    if not isinstance(s, dict):
        continue
    if pat and pat not in name:
        continue
    has_recipe = "recipe" in s and s["recipe"]
    mark = " " if has_recipe else "*"
    status = s.get("status", "")
    print(f"  [{mark}] {name:<55} {status}")
print()
print("(* = no recipe — scenario is a marker / blocked / pre-v2 stub)")
PYEOF
    exit 0
fi

# ── detect: do any matched scenarios need the VM? ───────────────
# Only run the SSH preflight + bootstrap for scenarios that actually
# need the VM. Pure host-side recipes don't.
NEEDS_VM=0
if [[ -f "${matrix_full}" ]]; then
    NEEDS_VM=$(python3 - "${matrix_full}" "${SCENARIO}" <<'PYEOF'
import json, sys
m = json.load(open(sys.argv[1]))
pat = sys.argv[2]
needs_vm = False
for name, s in m.get("scenarios", {}).items():
    if not isinstance(s, dict): continue
    if pat and pat not in name: continue
    for step in s.get("recipe", []):
        if not isinstance(step, dict): continue
        op = step.get("op") or step.get("type") or ""
        # Built-in ship ops imply VM. Per-step host override = vm.
        if op in ("ship-to-vm", "ship-to-host"):
            needs_vm = True; break
        if step.get("host") == "vm":
            needs_vm = True; break
    if needs_vm: break
print("1" if needs_vm else "0")
PYEOF
)
fi

# ── bootstrap: ensure .test-env exists + populated (only if VM needed) ──
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
export SSH_KEY="${SSH_KEY:-}"
EOF
    local gitignore="${consumer_root}/.gitignore"
    if [[ -f "${gitignore}" ]] && ! grep -qxF '.test-env' "${gitignore}"; then
        echo '.test-env' >> "${gitignore}"
        echo "[run-tests] added .test-env to .gitignore"
    fi
    echo "[run-tests] wrote ${ENV_FILE}"
}

if [[ "${NEEDS_VM}" == "1" ]]; then
    if [[ -f "${ENV_FILE}" ]]; then
        # shellcheck disable=SC1090
        source "${ENV_FILE}"
        # Back-compat: derive SSH_KEY from SSH_OPTS for older .test-env files
        # (pre-v2 dispatch — wrote SSH_OPTS with -i flag inline).
        if [[ -z "${SSH_KEY:-}" && "${SSH_OPTS:-}" == *"-i "* ]]; then
            SSH_KEY="$(echo "${SSH_OPTS}" | sed -n 's/.*-i \([^ ]*\).*/\1/p')"
        fi
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

    # Apply per-run overrides.
    [[ -n "${ARG_VM_HOST}"      ]] && VM_HOST="${ARG_VM_HOST}"
    [[ -n "${ARG_VM_WORKDIR}"   ]] && VM_WORKDIR="${ARG_VM_WORKDIR}"
    [[ -n "${ARG_VM_IMAGE_DIR}" ]] && VM_IMAGE_DIR="${ARG_VM_IMAGE_DIR}"
    if [[ -n "${ARG_SSH_KEY}" ]]; then
        SSH_KEY="${ARG_SSH_KEY}"
        build_ssh_opts_from_key
    fi
    if [[ -f "${ENV_FILE}" \
          && ( -n "${ARG_VM_HOST}" || -n "${ARG_VM_WORKDIR}" \
               || -n "${ARG_VM_IMAGE_DIR}" || -n "${ARG_SSH_KEY}" ) ]]; then
        write_env_file
    fi

    if [[ -z "${VM_HOST:-}" ]]; then
        echo "[run-tests] VM_HOST not set after bootstrap (this is a bug)" >&2
        exit 2
    fi

    # Export for the runner: dispatch.rs run_vm + run_builtin_ship
    # honour these env vars over harness.toml [vm].host / .ssh_key.
    export VM_HOST VM_WORKDIR VM_IMAGE_DIR SSH_KEY SSH_OPTS

    # ── preflight: SSH reachability with actionable hints ───────────
    preflight_ssh() {
        local probe_out probe_rc
        # shellcheck disable=SC2086
        probe_out=$(ssh ${SSH_OPTS:-} -o BatchMode=yes -o ConnectTimeout=5 \
            "${VM_HOST}" 'echo OK' 2>&1)
        probe_rc=$?
        if [[ "${probe_rc}" -eq 0 && "${probe_out}" == *OK* ]]; then
            return 0
        fi
        echo "[run-tests] preflight: SSH to ${VM_HOST} failed (rc=${probe_rc})" >&2
        echo "[run-tests] preflight: ${probe_out}" >&2

        # Auto-recover: missing known_hosts entry.
        if [[ "${probe_out}" == *"Host key verification failed"* \
           || "${probe_out}" == *"No matching host key"* ]]; then
            local ip="${VM_HOST#*@}"
            echo "[run-tests] preflight: auto-trusting ${ip}'s host key (ssh-keyscan -> ~/.ssh/known_hosts)" >&2
            if ssh-keyscan -H -T 5 "${ip}" >> "${HOME}/.ssh/known_hosts" 2>/dev/null; then
                # shellcheck disable=SC2086
                probe_out=$(ssh ${SSH_OPTS:-} -o BatchMode=yes -o ConnectTimeout=5 \
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
        echo "[run-tests] preflight failed; aborting before run" >&2
        exit 2
    fi
fi

# ── optional build phase ────────────────────────────────────────
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

# ── run phase: cargo run --bin run-matrix on the orchestrator ───
# Image-dir resolution priority for host-side ops:
#   HARNESS_IMAGE_DIR env (caller override) >
#   VM_IMAGE_DIR from .test-env >
#   [run].image_dir from harness.toml (host-side path) >
#   [vm].image_dir from harness.toml (default; usually points at the
#     VM-relative path)
: "${HARNESS_IMAGE_DIR:=${VM_IMAGE_DIR:-$(harness_get_or run.image_dir "$(harness_get_or vm.image_dir '')")}}"
export HARNESS_IMAGE_DIR
export HARNESS_CONSUMER_ROOT="${consumer_root}"

cd "${consumer_root}"
EXTRA_ARGS=""
[[ -n "${SCENARIO}" ]] && EXTRA_ARGS=$(printf ' %q' "${SCENARIO}")

echo "[run]  cargo run --bin run-matrix locally"
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
