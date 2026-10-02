#!/usr/bin/env bash
# tests/remote-timeout.sh -- a remote command that never returns is a named,
# bounded failure, not a silent hang.
#
# run-tests.sh talks to the Windows VM through many short SSH sessions: the
# VM-workdir lock (Acquire / Verify / Renew / Release), the ship phase, and
# the runner's one session per vm-step. On CI one of those sessions has
# stopped answering three times, and each time the job printed nothing until
# GitHub cancelled it 40 minutes (or six hours) later.
#
# This test puts a stand-in `ssh` first on PATH. It answers the preflight
# probe, then NEVER RETURNS from whichever VM-lock action the case names --
# exactly the shape of those hangs -- and asserts that run-tests.sh:
#
#   * fails, well inside a bound far shorter than the hang;
#   * names the command that stopped, and the scenario it was running for;
#   * leaves no stand-in ssh behind.
#
# A stand-in `cargo` stands in for the runner, so the run reaches the
# end-of-run Release without building anything. The runner's own per-step
# bound is covered by its unit tests (runner/src/dispatch.rs).
#
# Compatible with macOS bash 3.2. Needs python3 (as run-tests.sh does).

set -uo pipefail

HARNESS_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fswth-remote-timeout.XXXXXX")"

PASS=0
FAIL=0
ok()  { printf '  PASS  %s\n' "$1"; PASS=$((PASS + 1)); }
bad() { printf '  FAIL  %s\n' "$1"; FAIL=$((FAIL + 1)); }

# Every stand-in that was started records its pid; whatever is left at the
# end is killed, so a failing case cannot leak a sleeper into the next one.
kill_stand_ins() {
    local log="$1" pid
    [[ -f "${log}" ]] || return 0
    while read -r pid _; do
        kill -KILL "${pid}" 2>/dev/null || true
    done < "${log}"
}
cleanup() {
    local log
    for log in "${WORK_DIR}"/*/ssh.log; do kill_stand_ins "${log}"; done
    rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

# A run that hung has children of its own (the lease heartbeat, command
# substitutions); kill the whole tree, deepest first.
kill_tree() {
    local child
    for child in $(pgrep -P "$1" 2>/dev/null); do kill_tree "${child}"; done
    kill -KILL "$1" 2>/dev/null || true
}

# ── stand-ins ───────────────────────────────────────────────────
BIN="${WORK_DIR}/bin"
mkdir -p "${BIN}"

cat > "${BIN}/ssh" <<'PY'
#!/usr/bin/env python3
# Stand-in for ssh to the Windows VM. Works out which VM-lock action the
# command carries and either answers it at once or never returns.
import base64, json, os, re, sys, time

argv = sys.argv[1:]
command = argv[-1] if argv else ""
if command == "echo OK":                     # run-tests.sh's preflight probe
    print("OK")
    sys.exit(0)

action = "other"
encoded = re.search(r"-EncodedCommand\s+(\S+)", command)
if encoded:
    script = base64.b64decode(encoded.group(1)).decode("utf-16le")
    for blob in re.findall(r"FromBase64String\('([A-Za-z0-9+/=]+)'\)", script):
        try:
            action = json.loads(base64.b64decode(blob))["action"]
            break
        except Exception:
            continue
else:
    named = re.search(r"-Action\s+'?(\w+)", command)
    if named:
        action = named.group(1)

with open(os.environ["STAND_IN_LOG"], "a") as log:
    log.write("%d %s\n" % (os.getpid(), action))

if action == os.environ.get("STAND_IN_HANG", ""):
    while True:                              # never returns
        time.sleep(3600)
if action == "Acquire":
    print("[run-tests] VM matrix lock acquired: stand-in")
sys.exit(0)
PY

cat > "${BIN}/scp" <<'SH'
#!/usr/bin/env bash
exit 0
SH

cat > "${BIN}/cargo" <<'SH'
#!/usr/bin/env bash
echo "stand-in runner: nothing to run"
exit 0
SH
chmod +x "${BIN}/ssh" "${BIN}/scp" "${BIN}/cargo"

# ── a consumer with one vm-side scenario ────────────────────────
make_consumer() {
    local dir="$1"
    mkdir -p "${dir}"
    cat > "${dir}/fs-windows-test-harness.toml" <<'TOML'
[project]
name        = "remote-timeout-fixture"
matrix_path = "test-matrix.json"

[vm]
host      = "${VM_HOST}"
workdir   = "${VM_WORKDIR}"
image_dir = "images"

[ops.vm-wait]
host        = "vm"
command     = "Start-Sleep -Seconds 1"
expect_exit = 0
TOML
    cat > "${dir}/test-matrix.json" <<'JSON'
{
  "scenarios": {
    "remote-never-returns": {
      "status": "pending",
      "recipe": [ { "op": "vm-wait" } ]
    }
  }
}
JSON
}

# ── one case: hang at <action>, expect a named failure within the bound ──
# The remote bound is 2 s; the case gives the whole run 30 s. A run that is
# still going at 30 s is the hang this test exists to catch.
CASE_LIMIT=30
case_hangs_at() {
    local action="$1" dir="${WORK_DIR}/${1}"
    make_consumer "${dir}"
    : > "${dir}/ssh.log"

    local started=${SECONDS} rc out
    (
        cd "${dir}" || exit 99
        PATH="${BIN}:${PATH}" STAND_IN_LOG="${dir}/ssh.log" STAND_IN_HANG="${action}" \
        FSWTH_REMOTE_TIMEOUT_SECONDS=2 \
            exec bash "${HARNESS_ROOT}/scripts/run-tests.sh" remote-never-returns --no-ship \
                --vm-host=tester@stand-in --vm-workdir=C:/fswth-remote-timeout
    ) > "${dir}/out.txt" 2>&1 < /dev/null &
    local run_pid=$!

    while kill -0 "${run_pid}" 2>/dev/null && (( SECONDS - started < CASE_LIMIT )); do
        sleep 1
    done
    if kill -0 "${run_pid}" 2>/dev/null; then
        kill_tree "${run_pid}"
        wait "${run_pid}" 2>/dev/null
        kill_stand_ins "${dir}/ssh.log"
        bad "a hang at VM lock ${action} fails within ${CASE_LIMIT}s (still running; output: $(tr '\n' '|' < "${dir}/out.txt"))"
        return
    fi
    wait "${run_pid}"; rc=$?
    out="$(cat "${dir}/out.txt")"
    ok "a hang at VM lock ${action} ends within ${CASE_LIMIT}s ($((SECONDS - started))s)"

    if [[ "${rc}" -ne 0 ]]; then
        ok "a hang at VM lock ${action} fails the run (rc=${rc})"
    else
        bad "a hang at VM lock ${action} fails the run (rc=0)"
    fi
    if grep -q "TIMEOUT" <<< "${out}" && grep -q "VM lock ${action}" <<< "${out}"; then
        ok "the failure names the command: VM lock ${action}"
    else
        bad "the failure names the command: VM lock ${action} (output: $(tr '\n' '|' <<< "${out}"))"
    fi
    if grep "TIMEOUT" <<< "${out}" | grep -q "remote-never-returns"; then
        ok "the failure names the scenario it was running for"
    else
        bad "the failure names the scenario it was running for"
    fi

    local pid action_seen left=""
    while read -r pid action_seen; do
        if kill -0 "${pid}" 2>/dev/null; then left="${left} ${pid}(${action_seen})"; fi
    done < "${dir}/ssh.log"
    if [[ -z "${left}" ]]; then
        ok "no stand-in ssh is left running after a hang at ${action}"
    else
        bad "no stand-in ssh is left running after a hang at ${action}:${left}"
    fi
}

# The three places CI has hung: taking the lock, checking it before the
# ship, and giving it back at the end of the run.
case_hangs_at Acquire
case_hangs_at Verify
case_hangs_at Release

echo "==============================================================="
echo "  results: ${PASS} passed, ${FAIL} failed"
echo "==============================================================="
[[ ${FAIL} -eq 0 ]]
