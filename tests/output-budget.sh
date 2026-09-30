#!/usr/bin/env bash
#
# output-budget.sh -- the wrapper comes from rust-fs-core, the refusals are
# loud, and every task is budgeted.
#
# WHAT THIS DOES NOT TEST. The wrapper's own behaviour suite lives in
# rust-fs-core (tests/output_budget.rs), where the script does. Duplicating it
# here would be the committed copy again in another form: two suites agreeing
# with each other and nothing comparing them. What this repository owns is the
# resolution -- which script runs, and what happens when none can be found --
# the handful of properties scripts/task.sh actually depends on, and the rule
# that a task without a budget is not a task.
#
# WHY THE REFUSALS ARE DRIVEN AND NOT READ. A refusal nobody executes has
# never been shown to happen. Both of them are here: core absent, and core
# present but answering the wrong API version.
set -uo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESOLVE="$REPO/scripts/resolve-output-budget.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
pass=0; fail=0

ok()  { pass=$((pass + 1)); printf '  ok    %s\n' "$1"; }
bad() { fail=$((fail + 1)); printf '  FAIL  %s\n' "$1"; }
check_eq() { [ "$1" = "$2" ] && ok "$3" || bad "$3 (got '$1', want '$2')"; }
check_contains() { case "$1" in *"$2"*) ok "$3" ;; *) bad "$3 (output: ${1//$'\n'/ | })" ;; esac; }
check_missing() { case "$1" in *"$2"*) bad "$3" ;; *) ok "$3" ;; esac; }

# A stand-in for core's script: it answers --version and nothing else. The
# point of these is the resolver's decision, not the wrapper's behaviour.
stub_core() { # stub_core DIR VERSION-LINE
    mkdir -p "$1/scripts"
    {
        echo '#!/usr/bin/env bash'
        printf 'if [ "${1:-}" = --version ]; then echo "%s"; exit 0; fi\n' "$2"
        echo 'echo "stub core wrapper ran: $*"'
    } > "$1/scripts/output-budget.sh"
    chmod +x "$1/scripts/output-budget.sh"
}

unset OUTPUT_BUDGET_VERBOSE OUTPUT_BUDGET_FAIL_TAIL
echo "output-budget.sh"

# -------------------------------------------- there is no copy in this repo
[ -e "$REPO/scripts/output-budget.sh" ] \
    && bad "the wrapper is not committed here" \
    || ok "the wrapper is not committed here"

# The rename that the old copy's drift was hiding. The canonical script does
# not read the old name -- it reports it and carries on quiet -- so a file
# still setting it would silently do nothing. Only uses count, not prose: the
# name is assembled here so this file is not its own counter-example, and
# CHANGELOG.md is exempt because its released entries record what those
# versions did and rewriting them would be a different kind of lie.
needle="FWTH""_VERBOSE"
if ! git -C "$REPO" rev-parse --git-dir >/dev/null 2>&1; then
    bad "the old-name check needs a git checkout; there is no repository at $REPO"
else
    stale="$(git -C "$REPO" grep -InE "[\${}]$needle|$needle=" -- . ':!CHANGELOG.md')"
    [ -z "$stale" ] \
        && ok "no file still sets or reads the superseded verbose variable" \
        || bad "no file still sets or reads the superseded verbose variable (${stale//$'\n'/ | })"
fi

# ------------------------------------------------- FS_CORE_ROOT, and refusals
stub_core "$work/good" "rust-fs-core-output-budget 1"
out="$(FS_CORE_ROOT="$work/good" "$RESOLVE" 2>&1)"; rc=$?
check_eq "$rc" 0 "FS_CORE_ROOT with a valid wrapper resolves"
check_eq "$out" "$work/good/scripts/output-budget.sh" "and prints just the path"

mkdir -p "$work/empty"
out="$(FS_CORE_ROOT="$work/empty" "$RESOLVE" 2>&1)"; rc=$?
check_eq "$rc" 1 "an FS_CORE_ROOT with no wrapper is refused"
check_contains "$out" "FS_CORE_ROOT names '$work/empty'" "and the refusal names the path it was given"
check_contains "$out" "chore siblings" "and names what would provide it"

stub_core "$work/wrong" "rust-fs-core-output-budget 99"
out="$(FS_CORE_ROOT="$work/wrong" "$RESOLVE" 2>&1)"; rc=$?
check_eq "$rc" 1 "a wrapper answering another API version is refused"
check_contains "$out" "rust-fs-core-output-budget 1" "and says which version was expected"
check_contains "$out" "rust-fs-core-output-budget 99" "and which one it found"

stub_core "$work/noversion" ""
printf '#!/usr/bin/env bash\necho hello\n' > "$work/noversion/scripts/output-budget.sh"
out="$(FS_CORE_ROOT="$work/noversion" "$RESOLVE" 2>&1)"; rc=$?
check_eq "$rc" 1 "a wrapper with no --version at all is refused"

# ------------------------------------------ the sibling, and no fallback off it
# A checkout with a good core beside it. The resolver finds it with no
# FS_CORE_ROOT set -- and a bad FS_CORE_ROOT still fails rather than quietly
# using it, which is what "authoritative" has to mean for CI to be trusted.
mkdir -p "$work/tree/harness/scripts"
cp "$RESOLVE" "$work/tree/harness/scripts/"
stub_core "$work/tree/rust-fs-core" "rust-fs-core-output-budget 1"

out="$(cd "$work/tree/harness" && env -u FS_CORE_ROOT bash scripts/resolve-output-budget.sh 2>&1)"; rc=$?
check_eq "$rc" 0 "a sibling rust-fs-core is found with no FS_CORE_ROOT set"
check_contains "$out" "$work/tree/rust-fs-core/scripts/output-budget.sh" "and it is the sibling that is used"

out="$(cd "$work/tree/harness" && FS_CORE_ROOT="$work/empty" bash scripts/resolve-output-budget.sh 2>&1)"; rc=$?
check_eq "$rc" 1 "FS_CORE_ROOT is authoritative: a bad one does not fall back to a good sibling"
check_missing "$out" "$work/tree/rust-fs-core" "and the sibling is not silently substituted"

out="$(cd "$work/tree/harness" && env -u FS_CORE_ROOT bash scripts/resolve-output-budget.sh --core-dir 2>&1)"; rc=$?
check_eq "$out" "$work/tree/rust-fs-core" "--core-dir says where core is, for scripts/siblings.sh"

# ------------------------------------------------ this checkout's real core
# Not a stub: the core this repository's tasks will actually run. If it is
# absent the suite FAILS -- there is nothing here to skip, because a harness
# that cannot resolve its wrapper cannot run a budgeted task at all.
real="$("$RESOLVE" 2>&1)"; rc=$?
if [ "$rc" != 0 ]; then
    bad "this checkout resolves a real rust-fs-core (run 'chore siblings', or set FS_CORE_ROOT)"
    printf '%s\n' "$real"
    printf '\n%d passed, %d failed\n' "$pass" "$((fail + 1))"
    exit 1
fi
ok "this checkout resolves a real rust-fs-core"
check_eq "$(bash "$real" --version)" "rust-fs-core-output-budget 1" "and it speaks the pinned API version"

# ------------------------------- the four properties scripts/task.sh relies on
out="$("$REPO/scripts/task.sh" selftest selftest 50 4000 -- sh -c 'echo kumquat; echo two' 2>&1)"; rc=$?
check_eq "$rc" 0 "a task inside its budget succeeds"
check_contains "$out" "selftest: ok (2 lines" "and prints a verdict saying what it printed"
check_missing "$out" "kumquat" "and keeps the run itself off the terminal"

out="$("$REPO/scripts/task.sh" selftest selftest 1 4000 -- sh -c 'echo a; echo b' 2>&1)"; rc=$?
check_eq "$rc" 65 "a task that passed but printed too much exits 65"

out="$("$REPO/scripts/task.sh" selftest selftest 50 4000 -- sh -c 'echo the real error; exit 3' 2>&1)"; rc=$?
check_eq "$rc" 3 "a failing command exits with ITS status, not the wrapper's"
check_missing "$out" "the real error" "and a failure is quiet by default, naming the log instead"
out="$(OUTPUT_BUDGET_FAIL_TAIL=5 "$REPO/scripts/task.sh" selftest selftest 50 4000 -- sh -c 'echo the real error; exit 3' 2>&1)"
check_contains "$out" "the real error" "while OUTPUT_BUDGET_FAIL_TAIL brings the tail back"

out="$(CLI_ARGS=' --verbose ' "$REPO/scripts/task.sh" selftest selftest 50 4000 -- sh -c 'echo streamed' 2>&1)"; rc=$?
check_eq "$rc" 0 "chore <task> -- --verbose succeeds"
check_contains "$out" "streamed" "and streams the run (the variable is OUTPUT_BUDGET_VERBOSE now)"

# ------------------------------------- no core, no task: the refusal is loud
# The one that matters most: without the wrapper a task must NOT run its
# command unbudgeted. The marker file proves the command never started.
out="$(FS_CORE_ROOT="$work/empty" "$REPO/scripts/task.sh" selftest selftest 50 4000 \
    -- sh -c "touch '$work/ran-anyway'" 2>&1)"; rc=$?
[ "$rc" != 0 ] && ok "a task with no resolvable wrapper fails" || bad "a task with no resolvable wrapper fails"
[ -e "$work/ran-anyway" ] \
    && bad "and does not run the command unbudgeted" \
    || ok "and does not run the command unbudgeted"
check_contains "$out" "FS_CORE_ROOT" "and says what was missing"

# ------------------------------------------------ every task is budgeted
# The rule, checked against this repository's own chores.yml: each task that
# runs a suite goes through scripts/task.sh with two non-zero budgets. A task
# added later without one is exactly how "quiet" rots, so it fails here.
#
# `siblings` is not in the list and cannot be: it is the task that fetches the
# wrapper, so budgeting it through the wrapper would be a task that can only
# run once it has already run. It prints one line of git plumbing.
for task in lint test state-machine config output-budget agents-core smoke; do
    block="$(awk -v t="  $task:" '
        $0 == t { inside = 1; next }
        inside && /^  [a-z][a-z:_-]*:$/ { inside = 0 }
        inside { print }
    ' "$REPO/chores.yml")"
    if [ -z "$block" ]; then bad "chores.yml has a '$task' task"; continue; fi
    # scripts/task.sh LABEL LOG MAX-LINES MAX-BYTES: a zero in either budget
    # is "no budget" to the wrapper, which is the shape refused here.
    if printf '%s\n' "$block" | grep -Eq 'scripts/task\.sh +[^ ]+ +[^ ]+ +[1-9][0-9]* +[1-9][0-9]*'; then
        ok "chore $task runs under a line and a byte budget"
    else
        bad "chore $task runs under a line and a byte budget"
    fi
done

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" = 0 ]
