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
# remote-relative path of the runner is computed below and used in the
# REMOTE_CMD so the same script works regardless of whether consumers
# vendor at ./harness, ./vendor/fs-test-harness, ./tools/harness, etc.
HARNESS_REL=""
case "${harness_root}" in
    "${consumer_root}"/*)
        # Already shipped as part of the consumer tar. Compute its path
        # relative to consumer_root so we know where the runner lives on
        # the remote.
        HARNESS_REL="${harness_root#"${consumer_root}/"}"
        ;;
    *)
        # Out-of-tree checkout. Push it under <VM_WORKDIR>/harness/ and
        # use that as the remote-relative path.
        HARNESS_REL="harness"
        echo "[push] harness -> ${VM_HOST}:${VM_WORKDIR}/${HARNESS_REL}"
        # shellcheck disable=SC2029
        ssh ${SSH_OPTS} "${VM_HOST}" "if (-not (Test-Path '${VM_WORKDIR_PS}\\${HARNESS_REL}')) { New-Item -ItemType Directory -Path '${VM_WORKDIR_PS}\\${HARNESS_REL}' -Force | Out-Null }"
        tar --exclude='./target' --exclude='./.git' \
            -C "${harness_root}" -cf - . | \
            ssh ${SSH_OPTS} "${VM_HOST}" "tar -xf - -C '${VM_WORKDIR}/${HARNESS_REL}'"
        ;;
esac

# Push the image_dir if it's outside the consumer tree. The runner
# resolves images relative to consumer_root + VM_IMAGE_DIR, so we tar
# the image_dir to the matching location on the VM. Skipped when
# VM_IMAGE_DIR is unset, empty, absolute, or a path inside consumer_root
# (in which case the consumer-source tar above already shipped it).
if [[ -n "${VM_IMAGE_DIR}" ]]; then
    case "${VM_IMAGE_DIR}" in
        /*|[A-Za-z]:[/\\]*) ;;  # absolute paths handled by the consumer
        *)
            local_image_dir="${consumer_root}/${VM_IMAGE_DIR}"
            if [[ -d "${local_image_dir}" ]]; then
                # Resolve to a normalised real path so we can detect "is
                # this inside consumer_root" reliably even with `..`.
                resolved_image_dir="$(cd "${local_image_dir}" 2>/dev/null && pwd -P || echo "")"
                resolved_consumer="$(cd "${consumer_root}" && pwd -P)"
                case "${resolved_image_dir}" in
                    "${resolved_consumer}"|"${resolved_consumer}"/*) ;;  # under consumer tree
                    "")
                        echo "[push] image_dir ${VM_IMAGE_DIR} did not resolve; skipping" >&2
                        ;;
                    *)
                        # Compute remote path = VM_WORKDIR / VM_IMAGE_DIR, normalised.
                        remote_image_dir="${VM_WORKDIR}/${VM_IMAGE_DIR}"
                        # Replace `/foo/../bar` -> `/bar` segments at most twice for
                        # the common `..` cases we hit in practice.
                        for _ in 1 2 3; do
                            remote_image_dir="$(echo "${remote_image_dir}" | sed 's:/[^/]*/\.\./:/:g')"
                        done
                        remote_image_dir_ps="${remote_image_dir//\//\\}"
                        echo "[push] images -> ${VM_HOST}:${remote_image_dir}"
                        # shellcheck disable=SC2029
                        ssh ${SSH_OPTS} "${VM_HOST}" "if (-not (Test-Path '${remote_image_dir_ps}')) { New-Item -ItemType Directory -Path '${remote_image_dir_ps}' -Force | Out-Null }"
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

# Build the libtest-mimic argv passthrough.
EXTRA_ARGS=""
VERBOSE_ENV_PREFIX=""
# macOS ships bash 3.2 where `${TEST_ARGS[@]}` and `${#TEST_ARGS[@]}` on
# an empty array are treated as unset under `set -u`. Guard with
# `${arr[@]+...}` and `$#` from the original positional args.
if [[ $# -gt 0 ]]; then
    for arg in "${TEST_ARGS[@]}"; do
        if [[ "$arg" == "--verbose" ]]; then
            VERBOSE_ENV_PREFIX="\$env:MATRIX_VERBOSE='1'; "
            echo "[run]  --verbose detected -- engaging per-step tree on remote"
        fi
    done
    EXTRA_ARGS=$(printf ' %q' "${TEST_ARGS[@]}")
fi

# Forward the test-image dir as a generic env var so the runner +
# run-scenario.ps1 can resolve per-scenario images relative to it.
IMAGE_DIR_ESCAPED="${VM_IMAGE_DIR//\\/\\\\}"

# Optional: a consumer-supplied PowerShell prefix appended before the
# cargo invocation. Read from harness.toml `[vm.env_prefix]` if present.
ENV_PREFIX="$(harness_get_or vm.env_prefix "")"

REMOTE_CMD="Set-Location '${VM_WORKDIR_PS}'; \$env:PATH=\"\$env:USERPROFILE\\.cargo\\bin;\$env:PATH\"; \$env:HARNESS_IMAGE_DIR='${IMAGE_DIR_ESCAPED}'; \$env:HARNESS_CONSUMER_ROOT='${VM_WORKDIR_PS}'; ${ENV_PREFIX} ${VERBOSE_ENV_PREFIX}cargo run --manifest-path ${HARNESS_REL}/runner/Cargo.toml --release --bin run-matrix -- --test-threads=1${EXTRA_ARGS}"

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
