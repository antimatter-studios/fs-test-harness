#!/usr/bin/env bash
# claim-scenario.sh -- atomic scenario claim from test-matrix.json.
#
# Generic, FS-agnostic. Reads the consumer project's matrix file (path
# resolved via $MATRIX_PATH env var, else <repo-root>/test-matrix.json).
#
# Usage:
#   bash <harness>/scripts/claim-scenario.sh "<session-name>"
#
# Env (optional):
#   MATRIX_PATH   absolute path to the test-matrix.json to mutate
#                 (default: $PWD/test-matrix.json -- run from the
#                 consumer's repo root).
#
# Output:
#   stdout: the claimed scenario name (one line) on success
#   exit 0 -- scenario claimed
#   exit 1 -- no pending scenarios available
#   exit 2 -- usage error / missing work list
#
# Atomicity:
#   The work list is rewritten via mktemp + mv (atomic on POSIX). To
#   defend against a race where two agents both compute "first pending
#   = X" and both atomically rewrite, we read back AFTER the rename and
#   verify our session won. If another session is recorded, retry with
#   the next scenario. At most 16 attempts before declaring "no
#   scenarios available".

set -euo pipefail

SESSION="${1:-}"
if [[ -z "${SESSION}" ]]; then
    echo "usage: $0 <session-name>" >&2
    exit 2
fi

WORK_LIST="${MATRIX_PATH:-${PWD}/test-matrix.json}"

if [[ ! -f "${WORK_LIST}" ]]; then
    echo "missing work list: ${WORK_LIST}" >&2
    echo "  set MATRIX_PATH or run from the consumer repo root" >&2
    exit 2
fi

claim_attempt() {
    local session="$1"
    local tmp
    tmp="$(mktemp "${WORK_LIST}.tmp.XXXXXX")"

    local picked
    picked="$(python3 - "${WORK_LIST}" "${tmp}" "${session}" <<'PY'
import json, sys
src, dst, session = sys.argv[1], sys.argv[2], sys.argv[3]
with open(src) as f:
    data = json.load(f)
picked = None
for name, entry in data.get("scenarios", {}).items():
    if entry.get("status") == "pending":
        picked = name
        entry["status"] = f"claimed-{session}"
        break
if picked is None:
    sys.exit(1)
with open(dst, "w") as f:
    json.dump(data, f, indent=2, ensure_ascii=False)
print(picked)
PY
    )" || { rm -f "${tmp}"; return 1; }

    mv "${tmp}" "${WORK_LIST}"

    local actual_status
    actual_status="$(python3 -c "
import json
with open('${WORK_LIST}') as f:
    data = json.load(f)
print(data['scenarios']['${picked}']['status'])
")"
    if [[ "${actual_status}" == "claimed-${session}" ]]; then
        echo "${picked}"
        return 0
    fi
    return 2
}

for attempt in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16; do
    if claim_attempt "${SESSION}"; then
        exit 0
    fi
    rc=$?
    if [[ ${rc} -eq 1 ]]; then
        exit 1
    fi
    sleep "$(awk "BEGIN { srand(); print rand() * 0.5 + 0.1 }")"
done

echo "claim-scenario.sh: 16 attempts failed -- treat as no scenarios available" >&2
exit 1
