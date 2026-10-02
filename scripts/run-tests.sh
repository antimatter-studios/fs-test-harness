#!/usr/bin/env bash
# run-tests.sh -- single entrypoint for the windows-VM test matrix.
#
# Wraps every step of the pipeline (bootstrap, preflight, build, run,
# diag) so that a single command does everything. No separate setup
# script to remember.
#
# Since fs-windows-test-harness 3.0.0 the runner only speaks v2 (recipe-based
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
#   bash <harness>/scripts/run-tests.sh [SCENARIO] --no-ship
#     Skip the ship phase. The default is to ship harness `scripts/vm/`
#     + consumer `scripts/vm/` + the consumer binary (or full source
#     when `[run].vm_build_command` is set) to the VM whenever the
#     selected scenario set has any vm-side recipe step. Use --no-ship
#     when you've pre-staged the VM manually (faster iteration during
#     debugging of a single scenario).
#
#   bash <harness>/scripts/run-tests.sh [SCENARIO] --reinstall
#     Nuclear bootstrap: scp setup-windows-vm.ps1 to the VM and
#     invoke it with -Reinstall. Uninstalls every package declared in
#     `harness.toml [vm.packages]`, then reinstalls each one with the
#     declared `custom_args` (e.g. WinFsp's `ADDLOCAL=F.Main,F.User,
#     F.Developer` for headers + .lib). Resets the rustup default
#     toolchain. Use when the VM's package state is partial / wrong
#     / unknown — easier to nuke and rebuild than to repair partial
#     installs (e.g. WinFsp installed runtime-only when the consumer
#     binary's bindgen needs F.Developer headers).
#     Continues with the normal ship + run flow after the bootstrap
#     completes; combine with --no-ship if you want bootstrap-only.
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
# VM exclusivity: after preflight, a VM-side run atomically claims a lock
# directory inside VM_WORKDIR before reinstall, ship, or scenario work. A
# live run renews its lease every minute; an expired one can be recovered.
# Every VM mutation checks the owner token; guarded commands prevent a
# replacement from taking over until the command finishes.
#
# Bounded remote commands: every command this script sends to the VM (the
# preflight probe, each VM-lock action, every ship and scp) runs under a
# per-command timeout, and so does every recipe step the runner dispatches.
# A command that never returns is stopped and the run fails with a line
# naming the command, the bound and the scenario filter -- instead of
# holding the run, silently, until CI cancels the job. The bounds:
#   FSWTH_REMOTE_TIMEOUT_SECONDS     (300)  lock actions, preflight, ship, scp
#   FSWTH_VM_SETUP_TIMEOUT_SECONDS   (3600) --reinstall, [run].vm_build_command
#   FSWTH_STEP_TIMEOUT_SECONDS       (900)  each recipe step (the runner's)
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

echo "[harness] fs-windows-test-harness $(harness_self_version)"

# ── arg parsing ─────────────────────────────────────────────────
DO_BUILD=0
DO_LIST=0
DO_RESET=0
DO_SHIP=1   # default: ship vm-side artefacts whenever NEEDS_VM=1; --no-ship opts out
DO_REINSTALL=0   # --reinstall: nuclear bootstrap — uninstall+reinstall every package via setup-windows-vm.ps1
SCENARIO=""
ARG_VM_HOST=""
ARG_VM_WORKDIR=""
ARG_VM_IMAGE_DIR=""
ARG_SSH_KEY=""
WRAPPER_TMP=""
VM_LOCK_HELD=0

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
        --no-ship)          DO_SHIP=0 ;;
        --reinstall)        DO_REINSTALL=1 ;;
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

# Every bounded command names the scenario filter it was running for.
HARNESS_TIMEOUT_CONTEXT="scenario filter '${SCENARIO:-<every scenario>}'"
REMOTE_TIMEOUT="$(harness_timeout_seconds FSWTH_REMOTE_TIMEOUT_SECONDS 300)" || exit 2
SETUP_TIMEOUT="$(harness_timeout_seconds FSWTH_VM_SETUP_TIMEOUT_SECONDS 3600)" || exit 2
harness_timeout_seconds FSWTH_STEP_TIMEOUT_SECONDS 900 > /dev/null || exit 2

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
    NEEDS_VM=$(python3 - "${matrix_full}" "${SCENARIO}" "${harness_toml}" <<'PYEOF'
import json, sys
try:
    import tomllib
except ImportError:
    import tomli as tomllib
m = json.load(open(sys.argv[1]))
pat = sys.argv[2]
with open(sys.argv[3], 'rb') as f:
    ops = tomllib.load(f).get('ops', {})
needs_vm = False
for name, s in m.get("scenarios", {}).items():
    if not isinstance(s, dict): continue
    if pat and pat not in name: continue
    for step in s.get("recipe", []):
        if not isinstance(step, dict): continue
        op = step.get("op") or step.get("type") or ""
        # Built-in ship ops imply VM. Otherwise match the runner's host
        # resolution: step override, then op host (default vm).
        if op in ("ship-to-vm", "ship-to-host"):
            needs_vm = True; break
        op_def = ops.get(op, {})
        op_host = op_def.get('host', 'vm') if isinstance(op_def, dict) else 'vm'
        if step.get('host') == 'vm' or (step.get('host') != 'host' and op_host == 'vm'):
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
# Generated by fs-windows-test-harness/scripts/run-tests.sh on $(date '+%Y-%m-%d %H:%M:%S').
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

# Source .test-env unconditionally if it exists — VM_IMAGE_DIR is
# needed even by host-only scenarios (the runner uses it to resolve
# `{image_dir}` for verifier ops). VM_HOST / SSH_KEY are unused on
# pure-host runs but exporting them is harmless.
if [[ -f "${ENV_FILE}" ]]; then
    # shellcheck disable=SC1090
    source "${ENV_FILE}"
    if [[ -z "${SSH_KEY:-}" && "${SSH_OPTS:-}" == *"-i "* ]]; then
        SSH_KEY="$(echo "${SSH_OPTS}" | sed -n 's/.*-i \([^ ]*\).*/\1/p')"
    fi
    # Build SSH_OPTS from SSH_KEY if key is set but opts are empty.
    # This handles manually-written .test-env files that have SSH_KEY
    # but not the auto-generated SSH_OPTS line.
    if [[ -n "${SSH_KEY:-}" && -z "${SSH_OPTS:-}" ]]; then
        build_ssh_opts_from_key
    fi
    export VM_HOST VM_WORKDIR VM_IMAGE_DIR SSH_KEY SSH_OPTS
fi

if [[ "${NEEDS_VM}" == "1" ]]; then
    if [[ -f "${ENV_FILE}" ]]; then
        # Already sourced above.
        :
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
        probe_out=$(harness_bounded "${REMOTE_TIMEOUT}" "preflight ssh ${VM_HOST} 'echo OK'" \
            ssh -n ${SSH_OPTS:-} -o BatchMode=yes -o ConnectTimeout=5 \
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
                probe_out=$(harness_bounded "${REMOTE_TIMEOUT}" "preflight ssh ${VM_HOST} 'echo OK'" \
                    ssh -n ${SSH_OPTS:-} -o BatchMode=yes -o ConnectTimeout=5 \
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

    # ── VM-workdir lock: flat scenario images require exclusivity ────
    # The consumer's VM-side image names are scoped only by scenario, and
    # cleanup may sweep every image in VM_WORKDIR. Claim the workdir before
    # reinstall/ship/run so two independent matrix processes cannot overwrite
    # or delete each other's images. The PowerShell helper travels inside the
    # SSH command itself, so acquiring the lock does not depend on a prior
    # ship phase.
    #
    # IT IS NOT STREAMED ON STDIN ANY MORE, and nothing here may read the
    # remote stdin. Up to v4.2.0 the helper was piped into
    # `[Console]::In.ReadToEnd()` on the VM, and on Windows OpenSSH (sshd ->
    # powershell.exe default shell -> powershell.exe) that read sometimes
    # never saw end-of-file: all three CI hangs on record stopped inside one
    # of these streamed lock calls -- Acquire (rust-fs-ntfs run 36887143883),
    # the Verify before the ship (36683514330) and the Release at the end
    # (36685662539) -- while the far more numerous guarded commands, which
    # never read stdin, did not hang once. So the helper is gzipped into the
    # encoded command (about 12,300 characters, well inside Windows' 32,767),
    # and lock calls run `ssh -n`.
    VM_LOCK_RUN_ID="$(python3 -c 'import time; print(time.time_ns())')"
    VM_LOCK_OWNER_HOST="$(hostname 2>/dev/null || printf unknown)"
    VM_LOCK_OWNER_PID="$$"
    VM_LOCK_OWNER_TOKEN="$(python3 -c 'import uuid; print(uuid.uuid4())')"
    VM_LOCK_LEASE_SECONDS="${FSWTH_VM_LEASE_SECONDS:-1800}"
    VM_LOCK_HEARTBEAT_SECONDS="${FSWTH_VM_HEARTBEAT_SECONDS:-60}"
    if ! [[ "${VM_LOCK_LEASE_SECONDS}" =~ ^[1-9][0-9]{0,4}$ \
        && "${VM_LOCK_HEARTBEAT_SECONDS}" =~ ^[1-9][0-9]{0,4}$ ]] \
        || (( VM_LOCK_LEASE_SECONDS < 3 || VM_LOCK_LEASE_SECONDS > 86400 \
              || VM_LOCK_HEARTBEAT_SECONDS < 1 \
              || VM_LOCK_HEARTBEAT_SECONDS * 3 > VM_LOCK_LEASE_SECONDS )); then
        echo '[run-tests] invalid VM lease timing: require 3..86400 seconds and heartbeat <= lease/3' >&2
        exit 2
    fi
    VM_LOCK_HELPER_PATH="${VM_WORKDIR%/}/.fswth-matrix.lock/${VM_LOCK_OWNER_TOKEN}/matrix-run-lock.ps1"
    export FSWTH_VM_LOCK_SCRIPT="${VM_LOCK_HELPER_PATH}" FSWTH_VM_LOCK_WORKDIR="${VM_WORKDIR}"
    export FSWTH_VM_LOCK_RUN_ID="${VM_LOCK_RUN_ID}" FSWTH_VM_LOCK_TOKEN="${VM_LOCK_OWNER_TOKEN}"
    export FSWTH_VM_LOCK_HOST="${VM_LOCK_OWNER_HOST}" FSWTH_VM_LOCK_PID="${VM_LOCK_OWNER_PID}"

    vm_lock_encoded_command() {
        local action="$1" command="${2:-}"
        python3 - "${action}" "${VM_WORKDIR}" "${VM_LOCK_RUN_ID}" \
            "${VM_LOCK_OWNER_HOST}" "${VM_LOCK_OWNER_PID}" \
            "${VM_LOCK_OWNER_TOKEN}" "${VM_LOCK_LEASE_SECONDS}" \
            "${VM_LOCK_HELPER_PATH}" "${command}" \
            "${harness_root}/scripts/vm/matrix-run-lock.ps1" <<'PYEOF'
import base64
import gzip
import json
import sys

keys = ("action", "workdir", "run_id", "owner_host", "owner_pid", "owner_token",
        "lease_seconds", "helper_path", "command")
params = dict(zip(keys, sys.argv[1:-1]))
params_b64 = base64.b64encode(json.dumps(params).encode("utf-8")).decode("ascii")
if params["action"] == "Invoke":
    # The token-scoped copy shipped after Acquire; keeps the command short.
    load = "$script = $params.helper_path"
else:
    with open(sys.argv[-1], "rb") as f:
        helper_b64 = base64.b64encode(gzip.compress(f.read(), mtime=0)).decode("ascii")
    load = rf'''$zipped = New-Object IO.MemoryStream(,[Convert]::FromBase64String('{helper_b64}'))
$gunzip = New-Object IO.Compression.GZipStream($zipped, [IO.Compression.CompressionMode]::Decompress)
$reader = New-Object IO.StreamReader($gunzip, [Text.Encoding]::UTF8)
$script = [ScriptBlock]::Create($reader.ReadToEnd())
$reader.Dispose()'''
command = rf'''
$paramsJson = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{params_b64}'))
$params = $paramsJson | ConvertFrom-Json
{load}
& $script `
    -Action $params.action `
    -Workdir $params.workdir `
    -RunId $params.run_id `
    -OwnerHost $params.owner_host `
    -OwnerPid $params.owner_pid `
    -OwnerToken $params.owner_token `
    -LeaseSeconds ([int]$params.lease_seconds) `
    -Command $params.command
if (-not $?) {{ exit 1 }}
'''
print(base64.b64encode(command.encode("utf-16le")).decode("ascii"))
PYEOF
    }

    # vm_lock_remote ACTION -- one VM-lock action, bounded, stdin closed.
    vm_lock_remote() {
        local action="$1" encoded
        encoded="$(vm_lock_encoded_command "${action}")"
        # shellcheck disable=SC2086,SC2029
        harness_bounded "${REMOTE_TIMEOUT}" "VM lock ${action} (ssh ${VM_HOST})" \
            ssh -n ${SSH_OPTS:-} "${VM_HOST}" \
            "powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand ${encoded}"
    }

    # vm_lock_invoke COMMAND [LABEL] [SECONDS] -- COMMAND under the lease,
    # bounded by SECONDS (default FSWTH_REMOTE_TIMEOUT_SECONDS). A timeout
    # names LABEL, or COMMAND itself when no label is given.
    vm_lock_invoke() {
        local encoded label="${2:-VM command: $1}" seconds="${3:-${REMOTE_TIMEOUT}}"
        encoded="$(vm_lock_encoded_command Invoke "$1")"
        # stdin remains available to commands such as tar -xf -.
        # shellcheck disable=SC2086,SC2029
        harness_bounded "${seconds}" "${label} (ssh ${VM_HOST})" \
            ssh ${SSH_OPTS:-} "${VM_HOST}" \
            "powershell -NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand ${encoded}"
    }

    vm_lock_ship_file() {
        local src="$1" dest="$2" staged dest_dir
        staged="${VM_WORKDIR%/}/.fswth-matrix.lock/${VM_LOCK_OWNER_TOKEN}/$(basename "${dest}")"
        dest_dir="$(dirname "${dest}")"
        vm_lock_remote Verify
        # The upload only touches an owner-scoped staging path. If this
        # owner expires during scp, the guarded move below rejects it.
        # shellcheck disable=SC2086
        harness_bounded "${REMOTE_TIMEOUT}" "scp ${src} -> ${VM_HOST}:${staged}" \
            scp ${SSH_OPTS:-} "${src}" "${VM_HOST}:${staged}"
        vm_lock_invoke "New-Item -ItemType Directory -Path '${dest_dir//\'/\'\'}' -Force | Out-Null; Move-Item -LiteralPath '${staged//\'/\'\'}' -Destination '${dest//\'/\'\'}' -Force"
    }

    cleanup_run() {
        local run_rc=$? release_rc=0
        trap - EXIT USR1
        set +e
        if [[ -n "${VM_LOCK_HEARTBEAT_PID:-}" ]]; then
            kill "${VM_LOCK_HEARTBEAT_PID}" 2>/dev/null || true
            wait "${VM_LOCK_HEARTBEAT_PID}" 2>/dev/null || true
        fi
        if [[ "${VM_LOCK_HELD}" == "1" ]]; then
            vm_lock_remote Release
            release_rc=$?
            if [[ "${release_rc}" -ne 0 ]]; then
                echo "[run-tests] failed to release VM matrix lock (rc=${release_rc})" >&2
                [[ "${run_rc}" -eq 0 ]] && run_rc="${release_rc}"
            fi
        fi
        [[ -n "${WRAPPER_TMP}" ]] && rm -f "${WRAPPER_TMP}"
        exit "${run_rc}"
    }

    echo "[run-tests] VM lock: acquiring ${VM_WORKDIR} (each VM command bounded: ${REMOTE_TIMEOUT}s, each step: $(harness_timeout_seconds FSWTH_STEP_TIMEOUT_SECONDS 900)s)"
    set +e
    VM_LOCK_OUTPUT="$(vm_lock_remote Acquire 2>&1)"
    VM_LOCK_RC=$?
    set -e
    [[ -n "${VM_LOCK_OUTPUT}" ]] && printf '%s\n' "${VM_LOCK_OUTPUT}"
    if [[ "${VM_LOCK_RC}" -ne 0 ]]; then
        exit "${VM_LOCK_RC}"
    fi
    VM_LOCK_HELD=1
    trap cleanup_run EXIT
    VM_LOCK_HEARTBEAT_LOG="${consumer_root}/test-diagnostics/vm-lease-heartbeat.log"
    mkdir -p "$(dirname "${VM_LOCK_HEARTBEAT_LOG}")"
    : > "${VM_LOCK_HEARTBEAT_LOG}"
    trap 'echo "[run-tests] VM lease renewal failed; see ${VM_LOCK_HEARTBEAT_LOG}" >&2; exit 74' USR1
    # Use the orchestrator PID only for signalling this local process. It is
    # never consulted by the VM when deciding whether to reclaim a lease.
    VM_LOCK_PARENT_PID="${BASHPID}"
    (
        while sleep "${VM_LOCK_HEARTBEAT_SECONDS}"; do
            if ! vm_lock_remote Renew >/dev/null; then
                kill -USR1 "${VM_LOCK_PARENT_PID}" 2>/dev/null || true
                break
            fi
        done
    # A remote SSH child may outlive this subshell after cleanup kills it.
    # Keep its stderr off the caller's pipe so tee can observe EOF.
    ) > "${VM_LOCK_HEARTBEAT_LOG}" 2>&1 &
    VM_LOCK_HEARTBEAT_PID=$!
    vm_lock_remote Verify
    # The runner's VM commands invoke this token-scoped copy under the
    # Windows operation gate. Its path disappears on release/recovery.
    # shellcheck disable=SC2086
    harness_bounded "${REMOTE_TIMEOUT}" "scp matrix-run-lock.ps1 -> ${VM_HOST}:${VM_LOCK_HELPER_PATH}" \
        scp ${SSH_OPTS:-} "${harness_root}/scripts/vm/matrix-run-lock.ps1" \
        "${VM_HOST}:${VM_LOCK_HELPER_PATH}"
    vm_lock_remote Verify

    # ── --reinstall: scp setup-windows-vm.ps1 + ssh-invoke ──────
    # Nuclear bootstrap: uninstall+reinstall every package declared
    # in [vm.packages] (with their custom_args), reset rustup default
    # toolchain. Use when the VM's package state is partial / wrong
    # / unknown and you want to start over from a clean install.
    if [[ "${DO_REINSTALL}" == "1" ]]; then
        VM_WORKDIR_PS="${VM_WORKDIR//\//\\}"

        # Build a wrapper.ps1 locally with [vm.packages] from
        # harness.toml baked in. scp + run as a file rather than
        # pass through ssh-cmd-powershell argv (multiple layers of
        # quoting eat single-quoted PS literals; baking into a file
        # localises the escaping to the python heredoc here).
        WRAPPER_TMP=$(mktemp -t reinstall-wrapper.XXXXXX)

        PACKAGES_PS=$(python3 - "${harness_toml}" <<'PYEOF'
import sys, json
try:
    import tomllib
except ImportError:
    import tomli as tomllib  # noqa
with open(sys.argv[1], 'rb') as f:
    cfg = tomllib.load(f)
pkgs = cfg.get('vm', {}).get('packages', [])
parts = []
for p in pkgs:
    if isinstance(p, str):
        # Single-quoted PS string literal; PS escapes ' as ''.
        parts.append("'" + p.replace("'", "''") + "'")
    elif isinstance(p, dict) and p.get('id'):
        pid = p['id'].replace("'", "''")
        ca  = p.get('custom_args', '').replace("'", "''")
        parts.append("@{ id = '" + pid + "'; custom_args = '" + ca + "' }")
print('@(' + ', '.join(parts) + ')' if parts else '@()')
PYEOF
)
        TC_PS="$(harness_get_or vm.rust_toolchain '')"
        TC_PS="${TC_PS//\'/\'\'}"
        VM_WORKDIR_PS_QUOTED="${VM_WORKDIR_PS//\'/\'\'}"

        # Generate wrapper.ps1 — splat all args into setup-windows-vm.ps1.
        {
            echo '$invokeArgs = @{'
            echo "    Workdir       = '${VM_WORKDIR_PS_QUOTED}'"
            echo '    Reinstall     = $true'
            echo "    ExtraPackages = ${PACKAGES_PS}"
            if [[ -n "${TC_PS}" ]]; then
                echo "    RustToolchain = '${TC_PS}'"
            fi
            echo '}'
            echo '& "$PSScriptRoot\setup-windows-vm.ps1" @invokeArgs'
        } > "${WRAPPER_TMP}"

        echo "[reinstall] wrapper.ps1:"
        sed 's/^/  /' "${WRAPPER_TMP}"

        # Ensure VM workdir exists, then scp setup-windows-vm.ps1 +
        # the wrapper + invoke.
        # shellcheck disable=SC2086,SC2029
        vm_lock_ship_file "${harness_root}/scripts/setup-windows-vm.ps1" "${VM_WORKDIR}/setup-windows-vm.ps1"
        vm_lock_ship_file "${WRAPPER_TMP}" "${VM_WORKDIR}/reinstall-wrapper.ps1"

        echo "[reinstall] invoking setup-windows-vm.ps1 -Reinstall on ${VM_HOST}"
        # shellcheck disable=SC2086,SC2029
        vm_lock_invoke "powershell -ExecutionPolicy Bypass -File '${VM_WORKDIR_PS}\\reinstall-wrapper.ps1'" \
            "setup-windows-vm.ps1 -Reinstall" "${SETUP_TIMEOUT}"
        REINSTALL_RC=$?
        if [[ "${REINSTALL_RC}" -ne 0 ]]; then
            echo "[reinstall] setup-windows-vm.ps1 failed (rc=${REINSTALL_RC})" >&2
            exit "${REINSTALL_RC}"
        fi
        echo "[reinstall] complete"
    fi

    # ── ship phase: vm-side scripts + binary (or full source) ───
    # Idempotent. Always runs when NEEDS_VM=1 unless --no-ship was passed.
    if [[ "${DO_SHIP}" == "1" ]]; then
        VM_WORKDIR_PS="${VM_WORKDIR//\//\\}"
        ssh_run() {
            vm_lock_invoke "$*"
        }
        ship_dir() {
            # ship_dir <local-src> <vm-dest> — tar-pipe a directory tree.
            local src="$1" dest="$2"
            local dest_ps="${dest//\//\\}"
            ssh_run "if (-not (Test-Path '${dest_ps}')) { New-Item -ItemType Directory -Path '${dest_ps}' -Force | Out-Null }"
            tar -C "${src}" -cf - . | vm_lock_invoke "tar -xf - -C '${dest}'" "ship ${src} -> ${dest}"
        }
        ship_file() {
            # ship_file <local-src> <vm-dest> — single-file scp.
            vm_lock_ship_file "$1" "$2"
        }

        # Harness scripts/vm/ -> the VM-side harness root the runner
        # substitutes as {vm.harness_root}: VM workdir joined with
        # HARNESS_DIR from .test-env, else the sibling checkout
        # ../fs-windows-test-harness (runner/src/dispatch.rs
        # build_flat_vocab resolves it the same way). Shipping it keeps
        # the VM's op scripts at the orchestrator's harness version
        # instead of whatever was last copied there by hand.
        VM_HARNESS_ROOT="${VM_WORKDIR%/}/${HARNESS_DIR:-../fs-windows-test-harness}"
        echo "[ship] harness scripts/vm/ -> ${VM_HARNESS_ROOT}/scripts/vm"
        ship_dir "${harness_root}/scripts/vm" "${VM_HARNESS_ROOT}/scripts/vm"

        # Consumer scripts directory (declared in fs-windows-test-harness.toml
        # [vm].scripts_dir, default "scripts/fs-windows-test-harness" -- same
        # as VmSection::scripts_dir_or_default). Contains per-op PowerShell
        # helpers called by win-* ops.
        SCRIPTS_DIR="$(harness_get_or vm.scripts_dir "scripts/fs-windows-test-harness")"
        if [[ -d "${consumer_root}/${SCRIPTS_DIR}" ]]; then
            echo "[ship] consumer ${SCRIPTS_DIR}/"
            ship_dir "${consumer_root}/${SCRIPTS_DIR}" "${VM_WORKDIR}/${SCRIPTS_DIR}"
        fi

        # If [run].vm_build_command is set, ship the full source tree
        # (the build needs sources, not just a binary) and run the
        # build command on the VM. Otherwise ship just the prebuilt
        # binary path declared in [project].binary.
        VM_BUILD_COMMAND="$(harness_get_or run.vm_build_command "")"
        if [[ -n "${VM_BUILD_COMMAND}" ]]; then
            echo "[ship] consumer source tree (vm_build_command set)"
            # shellcheck disable=SC2086
            tar --exclude='./target' --exclude='./.git' --exclude='./.history' \
                --exclude='./test-diagnostics' --exclude='./diag' \
                --exclude='*.swp' --exclude='.DS_Store' \
                --exclude='./.test-env' \
                -C "${consumer_root}" -cf - . | \
                vm_lock_invoke "tar -xf - -C '${VM_WORKDIR}'" "ship consumer source tree -> ${VM_WORKDIR}"
            echo "[vm-build] ${VM_BUILD_COMMAND}"
            vm_lock_invoke "Set-Location '${VM_WORKDIR_PS}'; ${VM_BUILD_COMMAND}" \
                "[run].vm_build_command" "${SETUP_TIMEOUT}"
        else
            BINARY_REL="$(harness_get_or project.binary "")"
            if [[ -n "${BINARY_REL}" && -f "${consumer_root}/${BINARY_REL}" ]]; then
                echo "[ship] consumer binary ${BINARY_REL}"
                ship_file "${consumer_root}/${BINARY_REL}" "${VM_WORKDIR}/${BINARY_REL}"
            elif [[ -n "${BINARY_REL}" ]]; then
                echo "[ship] WARNING: [project].binary='${BINARY_REL}' not found at ${consumer_root}/${BINARY_REL}" >&2
                echo "[ship]          vm-side scenarios may fail. Set [run].vm_build_command to build on VM, or run --build first." >&2
            fi
        fi
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
# Hand the runner the config this script already resolved.
export HARNESS_TOML="${harness_toml}"

cd "${consumer_root}"
EXTRA_ARGS=""
[[ -n "${SCENARIO}" ]] && EXTRA_ARGS=$(printf ' %q' "${SCENARIO}")

# Derive --test-threads from max_parallel so the libtest-mimic thread pool
# is never the bottleneck. If max_parallel is "drive-letters" (or any
# non-integer), fall back to 26 (Windows A-Z upper bound). The runner
# clamps the actual semaphore to 1..=24 regardless.
_mp="$(harness_get_or runner.max_parallel "drive-letters")"
if [[ "${_mp}" =~ ^[0-9]+$ ]]; then
    TEST_THREADS="${_mp}"
else
    TEST_THREADS=26
fi

echo "[run]  cargo run --bin run-matrix locally"
echo "[run]  HARNESS_IMAGE_DIR=${HARNESS_IMAGE_DIR}"
echo "[run]  test_threads=${TEST_THREADS} (from max_parallel=${_mp})"
echo
set +e
cargo run --manifest-path "${harness_root}/runner/Cargo.toml" \
          --release --bin run-matrix -- --test-threads="${TEST_THREADS}"${EXTRA_ARGS}
RUN_EXIT=$?
set -e

echo
echo "==============================================================="
echo "diagnostics: ${consumer_root}/test-diagnostics/matrix/"
echo "test exit:   ${RUN_EXIT}  (0 = all passed/ignored; non-zero = at least one failed)"
echo "==============================================================="
exit ${RUN_EXIT}
