#!/usr/bin/env bash
# test-windows-matrix.sh -- Mac-side scaffold around the matrix testing core.
#
# Architecture:
#
#   +-- this script (Mac-side scaffold) ----------------+
#   | tar consumer source -> ssh -> invoke run-matrix  |
#   |   bin -> pull diag tree                          |
#   +-------------------+--------------------------------+
#                       |
#   +-- testing core (runs inside Windows) -------------+
#   | cargo run --release --bin run-matrix --           |
#   |   --test-threads=1 --no-fail-fast                 |
#   +---------------------------------------------------+
#
# Output: per-scenario PASS/FAIL listing from libtest-mimic plus an
# aggregate summary line. The full diag tree (per-scenario manifest.json,
# mount-stdout.txt, op-trace.jsonl, ...) lands at
# ${CONSUMER_ROOT}/test-diagnostics/run-<UTC>/.

set -euo pipefail

# shellcheck source=_lib_harness.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/_lib_harness.sh"

# Auto-load .test-env from the consumer repo root (written by setup-local.sh).
if [[ -f "${consumer_root}/.test-env" ]]; then
    # shellcheck disable=SC1091
    source "${consumer_root}/.test-env"
fi

if [[ -z "${VM_HOST:-}" ]]; then
    echo "VM_HOST is not set. Run scripts/setup-local.sh first," >&2
    echo "or export VM_HOST=user@host." >&2
    exit 2
fi

PROJECT_NAME="$(harness_get_or project.name "consumer")"

VM_WORKDIR="${VM_WORKDIR:-$(harness_get_or vm.workdir "C:/Users/${VM_HOST%%@*}/dev/${PROJECT_NAME}")}"
VM_IMAGE_DIR="${VM_IMAGE_DIR:-$(harness_get_or vm.image_dir "")}"
SSH_OPTS="${SSH_OPTS:-}"
# Always-on SSH timeouts so a dead VM fails fast.
SSH_OPTS="${SSH_OPTS} -o ConnectTimeout=10 -o ServerAliveInterval=15 -o ServerAliveCountMax=4"
VM_WORKDIR_PS="${VM_WORKDIR//\//\\}"

TIMESTAMP="$(date +%Y%m%d-%H%M%S)"
DIAG_BASE="${DIAG_DIR:-${consumer_root}/test-diagnostics}"
DIAG_LOCAL="${DIAG_BASE}/run-${TIMESTAMP}"
mkdir -p "${DIAG_LOCAL}"

# Optional libtest-mimic argv passthrough.
TEST_ARGS=("$@")

cd "${consumer_root}"

echo "[push] tar-ssh source -> ${VM_HOST}:${VM_WORKDIR}"
# shellcheck disable=SC2029
ssh ${SSH_OPTS} "${VM_HOST}" "if (-not (Test-Path '${VM_WORKDIR_PS}')) { New-Item -ItemType Directory -Path '${VM_WORKDIR_PS}' -Force | Out-Null }"

# Standard exclude list. Consumers may extend via $HARNESS_TAR_EXCLUDE
# (whitespace-separated additional --exclude args).
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

tar "${TAR_EXCLUDES[@]}" -cf - . | \
    ssh ${SSH_OPTS} "${VM_HOST}" "tar -xf - -C '${VM_WORKDIR}'"

# Also push the harness checkout itself unless the consumer wired it as a
# submodule already (the tar above will have included it then). The
# convention: harness lives at <consumer_root>/harness/. If $harness_root
# is not under $consumer_root, push it under <VM_WORKDIR>/harness/ so the
# remote can find scripts/run-scenario.ps1.
case "${harness_root}" in
    "${consumer_root}"/*) ;;  # already shipped as part of consumer tar
    *)
        echo "[push] harness -> ${VM_HOST}:${VM_WORKDIR}/harness"
        # shellcheck disable=SC2029
        ssh ${SSH_OPTS} "${VM_HOST}" "if (-not (Test-Path '${VM_WORKDIR_PS}\\harness')) { New-Item -ItemType Directory -Path '${VM_WORKDIR_PS}\\harness' -Force | Out-Null }"
        tar --exclude='./target' --exclude='./.git' \
            -C "${harness_root}" -cf - . | \
            ssh ${SSH_OPTS} "${VM_HOST}" "tar -xf - -C '${VM_WORKDIR}/harness'"
        ;;
esac

# Build the libtest-mimic argv passthrough.
EXTRA_ARGS=""
VERBOSE_ENV_PREFIX=""
for arg in "${TEST_ARGS[@]}"; do
    if [[ "$arg" == "--verbose" ]]; then
        VERBOSE_ENV_PREFIX="\$env:MATRIX_VERBOSE='1'; "
        echo "[run]  --verbose detected -- engaging per-step tree on remote"
    fi
done
if [[ ${#TEST_ARGS[@]} -gt 0 ]]; then
    EXTRA_ARGS=$(printf ' %q' "${TEST_ARGS[@]}")
fi

# Forward the test-image dir as a generic env var so the runner +
# run-scenario.ps1 can resolve per-scenario images relative to it.
IMAGE_DIR_ESCAPED="${VM_IMAGE_DIR//\\/\\\\}"

# Optional: a consumer-supplied PowerShell prefix appended before the
# cargo invocation. Read from harness.toml `[vm.env_prefix]` if present.
ENV_PREFIX="$(harness_get_or vm.env_prefix "")"

REMOTE_CMD="Set-Location '${VM_WORKDIR_PS}'; \$env:PATH=\"\$env:USERPROFILE\\.cargo\\bin;\$env:PATH\"; \$env:HARNESS_IMAGE_DIR='${IMAGE_DIR_ESCAPED}'; \$env:HARNESS_CONSUMER_ROOT='${VM_WORKDIR_PS}'; ${ENV_PREFIX} ${VERBOSE_ENV_PREFIX}cargo run --manifest-path harness/runner/Cargo.toml --release --bin run-matrix -- --test-threads=1${EXTRA_ARGS}"

echo "[run]  cargo run --bin run-matrix on ${VM_HOST}"
echo "[run]  remote: cargo run --bin run-matrix -- --test-threads=1${EXTRA_ARGS}"
echo
set +e
# shellcheck disable=SC2029
ssh ${SSH_OPTS} "${VM_HOST}" "${REMOTE_CMD}"
RUN_EXIT=$?
set -e

echo
echo "[pull] test-diagnostics -> ${DIAG_LOCAL}"
# Pull the matrix diag dir back. strip-components=2 strips
# "test-diagnostics/matrix/" so the local layout is
# test-diagnostics/run-<ts>/<scenario>/...
# shellcheck disable=SC2029
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
