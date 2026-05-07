#!/usr/bin/env bash
# reset-non-passed.sh -- reset every non-`passed-*` scenario back to
# `pending` so a new pass can re-claim them. Idempotent.
#
# Usage: bash <harness>/scripts/reset-non-passed.sh
#
# Env (optional):
#   MATRIX_PATH   absolute path to the test-matrix.json to mutate
#                 (default: $PWD/test-matrix.json).

set -euo pipefail
WORK_LIST="${MATRIX_PATH:-${PWD}/test-matrix.json}"
if [[ ! -f "${WORK_LIST}" ]]; then
    echo "missing work list: ${WORK_LIST}" >&2
    exit 2
fi

python3 - "${WORK_LIST}" <<'PY'
import json, sys
src = sys.argv[1]
with open(src) as f:
    d = json.load(f)
moved = 0
for name, e in d.get("scenarios", {}).items():
    s = e.get("status", "")
    if not s.startswith("passed-"):
        e["status"] = "pending"
        moved += 1
with open(src, "w") as f:
    json.dump(d, f, indent=2, ensure_ascii=False)
    f.write("\n")
print(f"reset {moved} scenarios to pending; {len(d['scenarios']) - moved} remain passed-*")
PY
