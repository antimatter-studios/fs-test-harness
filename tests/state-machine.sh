#!/usr/bin/env bash
# tests/state-machine.sh -- self-test for the claim/update/reset trio.
#
# Builds a synthetic test-matrix.json in a tempdir, then exercises each
# script against it. Asserts the expected status transitions, evidence
# link writeback, and the no-double-claim guarantee under concurrent
# claimers.
#
# Compatible with macOS bash 3.2 (no associative arrays, no mapfile).
# Run from any cwd; the script resolves the harness root via its own
# location.

set -euo pipefail

HARNESS_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK_DIR="$(mktemp -d -t fsharness-statemachine.XXXXXX)"
MATRIX="${WORK_DIR}/test-matrix.json"
export MATRIX_PATH="${MATRIX}"

cleanup() {
    rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

PASS=0
FAIL=0

assert() {
    local label="$1"
    local got="$2"
    local want="$3"
    if [[ "${got}" == "${want}" ]]; then
        printf '  PASS  %s\n' "${label}"
        PASS=$((PASS + 1))
    else
        printf '  FAIL  %s\n        got: %s\n        want: %s\n' \
            "${label}" "${got}" "${want}"
        FAIL=$((FAIL + 1))
    fi
}

assert_match() {
    local label="$1"
    local got="$2"
    local pattern="$3"
    if [[ "${got}" == ${pattern} ]]; then
        printf '  PASS  %s\n' "${label}"
        PASS=$((PASS + 1))
    else
        printf '  FAIL  %s\n        got: %s\n        want match: %s\n' \
            "${label}" "${got}" "${pattern}"
        FAIL=$((FAIL + 1))
    fi
}

status_of() {
    local name="$1"
    python3 - "${MATRIX}" "${name}" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
print(d["scenarios"][sys.argv[2]].get("status", ""))
PY
}

evidence_of() {
    local name="$1"
    python3 - "${MATRIX}" "${name}" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
print(d["scenarios"][sys.argv[2]].get("evidence_link", ""))
PY
}

write_fixture_matrix() {
    local count="$1"
    python3 - "${MATRIX}" "${count}" <<'PY'
import json, sys
path, n = sys.argv[1], int(sys.argv[2])
scenarios = {}
for i in range(n):
    scenarios[f"sc{i:02d}"] = {"status": "pending", "image": "fake.img"}
with open(path, "w") as f:
    json.dump({"_format": "v1", "scenarios": scenarios}, f, indent=2)
PY
}

echo "==============================================================="
echo "fs-test-harness state-machine self-test"
echo "  harness_root = ${HARNESS_ROOT}"
echo "  matrix       = ${MATRIX}"
echo "==============================================================="

# ---- 1. Single claim transitions pending -> claimed-<session> -------
echo "[1] single claim"
write_fixture_matrix 3
SESSION="agent-test-1"
CLAIMED="$(bash "${HARNESS_ROOT}/scripts/claim-scenario.sh" "${SESSION}")"
assert "claim returned a scenario name" "${CLAIMED:0:2}" "sc"
assert "claimed scenario status updated" \
    "$(status_of "${CLAIMED}")" "claimed-${SESSION}"

# ---- 2. update-status: claimed -> passed, evidence written -----------
echo "[2] update-scenario-status"
EVIDENCE="/tmp/fake/diag-${CLAIMED}"
bash "${HARNESS_ROOT}/scripts/update-scenario-status.sh" \
    "${CLAIMED}" "passed-${SESSION}" "${EVIDENCE}" >/dev/null
assert "status -> passed-<session>" \
    "$(status_of "${CLAIMED}")" "passed-${SESSION}"
assert "evidence_link recorded"  "$(evidence_of "${CLAIMED}")" "${EVIDENCE}"

# Failed-without-evidence path: status changes but evidence may stay or
# be appended. Just verify the status mutation here.
NEXT="$(bash "${HARNESS_ROOT}/scripts/claim-scenario.sh" "${SESSION}")"
bash "${HARNESS_ROOT}/scripts/update-scenario-status.sh" \
    "${NEXT}" "failed-${SESSION}" >/dev/null
assert "status -> failed-<session>" \
    "$(status_of "${NEXT}")" "failed-${SESSION}"

# ---- 3. reset-non-passed preserves passed-*, resets the rest ---------
echo "[3] reset-non-passed"
# State before: one passed-*, one failed-*, one pending. reset should
# leave the passed alone and move both others to pending.
PASSED_NAME="${CLAIMED}"
FAILED_NAME="${NEXT}"
bash "${HARNESS_ROOT}/scripts/reset-non-passed.sh" >/dev/null
assert "passed-* preserved" \
    "$(status_of "${PASSED_NAME}")" "passed-${SESSION}"
assert "failed-* reset to pending" \
    "$(status_of "${FAILED_NAME}")" "pending"

# Anything still pending stays pending.
ALL_REMAINING="$(python3 - "${MATRIX}" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
non_passed = [n for n, e in d["scenarios"].items()
              if not e.get("status", "").startswith("passed-")]
print(",".join(sorted(non_passed)))
PY
)"
NON_PASSED_PENDING="$(python3 - "${MATRIX}" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
ok = all(e.get("status", "") == "pending"
         for n, e in d["scenarios"].items()
         if not e.get("status", "").startswith("passed-"))
print("yes" if ok else "no")
PY
)"
assert "every non-passed scenario is pending after reset" \
    "${NON_PASSED_PENDING}" "yes"
echo "    (non-passed set: ${ALL_REMAINING})"

# ---- 4. concurrent claims: 8 parallel claimers --------------------
echo "[4] concurrent claims (8 parallel claimers, 6 pending)"
write_fixture_matrix 6
CONCURRENCY=8
PIDS=""
RES_DIR="${WORK_DIR}/concurrent-results"
mkdir -p "${RES_DIR}"

for i in 1 2 3 4 5 6 7 8; do
    (
        if out="$(bash "${HARNESS_ROOT}/scripts/claim-scenario.sh" \
                "agent-c${i}" 2>/dev/null)"; then
            echo "${out}" > "${RES_DIR}/${i}.ok"
        else
            echo "no-claim" > "${RES_DIR}/${i}.empty"
        fi
    ) &
    PIDS="${PIDS} $!"
done
# Bash 3.2-compatible wait-all: bare `wait` blocks until every
# background job exits; no PID list iteration needed.
wait

OK_COUNT="$(find "${RES_DIR}" -name '*.ok' -type f | wc -l | tr -d ' ')"
EMPTY_COUNT="$(find "${RES_DIR}" -name '*.empty' -type f | wc -l | tr -d ' ')"
assert "exactly 6 claims succeeded"  "${OK_COUNT}"    "6"
assert "exactly 2 claims failed (empty work list)" \
    "${EMPTY_COUNT}" "2"

# Union of names across .ok files == set of pending names == all 6.
CLAIMED_SET="$(cat "${RES_DIR}"/*.ok | sort -u | tr '\n' ',' | sed 's/,$//')"
EXPECTED_SET="sc00,sc01,sc02,sc03,sc04,sc05"
assert "no double-claims; union of claimers == fixture set" \
    "${CLAIMED_SET}" "${EXPECTED_SET}"

# Every scenario should be claimed-* now (no pending left).
NO_PENDING_LEFT="$(python3 - "${MATRIX}" <<'PY'
import json, sys
with open(sys.argv[1]) as f:
    d = json.load(f)
pending = [n for n, e in d["scenarios"].items()
           if e.get("status") == "pending"]
print("yes" if not pending else "no")
PY
)"
assert "no pending scenarios remain after 8 claimers" \
    "${NO_PENDING_LEFT}" "yes"

echo "==============================================================="
echo "  results: ${PASS} passed, ${FAIL} failed"
echo "==============================================================="
if [[ ${FAIL} -gt 0 ]]; then
    exit 1
fi
exit 0
