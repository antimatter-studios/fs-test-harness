# Working in fs-windows-test-harness (agent guide)

A reusable test harness for filesystem drivers that are developed on macOS or
Linux and have to run on Windows. The orchestrator (`scripts/run-tests.sh`)
tars a consumer's source to a Windows VM over SSH, the Rust runner in
`runner/` (`run-matrix`, one libtest-mimic trial per scenario) drives each
scenario there through PowerShell — mount an image, run ops on the drive
letter, unmount, optionally post-verify — and the diagnostics come back to the
host. Scenario state lives in the consumer's `test-matrix.json`, rewritten
atomically so several agents can share one matrix. It knows nothing about any
particular filesystem: the mount command, op templates and ready-line regex are
the consumer's, in its `fs-windows-test-harness.toml`.

This file is the fast path for an agent picking up work here. It points at the
existing docs rather than duplicating them:

- **README** → `## Self-test`, `## Output: quiet by default`,
  `## CI and automated merging` (and `### The smoke test`).
- **`docs/`** → `architecture.md` (the three layers, the scenario state
  machine, the adapter model), `consumer-integration.md` (the contract a
  consumer signs), `triage-protocol.md` (reading a red scenario),
  `multi-agent-protocol.md`, `vocabulary.md`, `vm-setup.md`.
- **CHANGELOG.md** → what each tag contains.
- **`.github-guard`** → the one required check, `ci-ok`.

The section between the BEGIN/END markers below is **shared, byte-identical,
with every repository in this family**. Do not edit it here: change the
canonical copy and propagate it, or `scripts/agents-core-check.sh` will fail.
Everything after the END marker is specific to this repository.

<!-- BEGIN SHARED BLOCK: agent-core v2 sha256:38af4d2c5377d38ab382baa4eab4aa679841e2b4eba4f4d01dacd255ffa7d32e -->
## Claiming work

Several agents work these repositories at the same time. Before you start on
an issue, claim it, so nobody else spends a session on what you are already
doing. The lock is a **GitHub label**, because labels are shared state that
every agent can read and change without posting comments into the thread.

**Before starting.** Check, claim, then read back:

```sh
gh issue view <N> --json labels                      # holds `claimed`? pick another
gh issue edit <N> --add-label claimed --add-label claim/<session>
gh issue view <N> --json labels                      # read back and confirm
```

`<session>` is your session name — `agent-<random4>-<isodate>`, e.g.
`agent-3f7c-2026-09-22`. Create the `claim/<session>` label if it does not
exist.

**Resolving a race.** Adding a label is not compare-and-swap: two agents can
both add `claimed` and both believe they won. That is what the read-back is
for. If it shows more than one `claim/*` label, the **lexically lowest**
session keeps the issue; every other agent removes its own `claim/*` label and
picks different work. Each racer computes the same answer independently, so no
further coordination is needed.

**When you finish or stop.** Remove both labels — on merge, or the moment you
abandon the work:

```sh
gh issue edit <N> --remove-label claimed --remove-label claim/<session>
```

Delete your `claim/<session>` label from the repository at the end of your
session so they do not accumulate.

**Reclaiming a stale claim.** An agent that dies holding a claim would block an
issue forever. If `claimed` was applied more than 12 hours ago and the holder's
branch has no commits since, any agent may take it: remove the stale `claim/*`,
add your own, and say so in the issue.

**This is a convention, not a fence.** Nothing enforces it. An agent that
ignores it duplicates work; it cannot corrupt anything. Honour it anyway.

## Work in a worktree

Every working copy is a **git worktree** of an existing checkout, made with
`git worktree add`. Never `git clone` a second, unlinked copy — not for a
branch, a PR, a review, or a sibling you need at another ref:

```sh
git -C <checkout> fetch origin
git -C <checkout> worktree add <path> -b <type>/<name> origin/main   # new work
git -C <checkout> worktree add --detach <path> <tag>                 # a sibling at a pinned ref
git -C <checkout> worktree remove <path>                             # when done
```

A worktree shares the checkout's objects and remotes, and `git worktree list`
shows it to every agent on the machine, so nobody else mistakes it for
abandoned work or loses track of it. An unlinked clone copies all the history
again, is invisible to that list, and gets left behind in `/tmp` long after the
work that made it is merged. Remove your worktree when you finish.

## Skills to use

- **`dev-loop`** — the required loop for any non-trivial change: baseline the
  full suite → change → re-run (no baseline test may regress) → enhance tests →
  vet. Always run it.
- **`commit`** / **`pr`** — for grouping commits and opening pull requests.

Each repository names any further skills of its own below.

## A bug fix starts with a red

**Prove it is broken first** — a failing check or test — *then* fix it, *then*
prove that same check is green, *then* confirm the full baseline still passes.
Never write the fix before you have a red. A fix with no failing test to its
name is a claim, not a result.

## Nothing skips

A test that cannot run **fails**, naming the task that would provide what it
needed. Never add an early return for a missing fixture, tool or VM: a skipped
test reads exactly like a passing one, and a suite that quietly declines to run
is indistinguishable from a suite that passes.

Where a tier reports skips or ignored tests, that is a gate, not a note.

## Validate against something that is not us

A driver's own readers share its interpretation of the format, so they cannot
catch a misreading: the mistake is baked into the fixture *and* the parser, and
they agree with each other while disagreeing with every real filesystem. Unit
tests over self-built fixtures prove self-consistency, not correctness.

Every structure that is parsed or written gets a cross-validation test against
an **independent oracle** — the platform's own tools, a real kernel, or a third
implementation — before it is considered done. Each repository names its
oracles below.

## Output is budgeted

Test tiers run through `scripts/tier.sh`, which runs the suite **quietly**: the
whole run goes to `tmp/logs/<tier>.log`, a pass prints one verdict line naming
that log, and a failure prints the verdict, the command's status and the log's
path — `--tail N`, or `OUTPUT_BUDGET_FAIL_TAIL=N`, prints the tail for whoever
is watching. **Read the log**: a failing tier names it and does not recite it.
CI keeps the logs as an artifact, so the detail is always retrievable.

The budget caps the log, not merely what is shown, and every number in the
table was measured. A run that passes but prints more than its budget **fails**.

The reader who pays most for a noisy suite is an agent that re-reads its whole
transcript on every step, and so pays for one loud run many times over. If a
tier legitimately grows, raise its row **with the measurement that justifies
it**. Do not silence output to fit, and do not route around `tier.sh`.

## Commits and branches

- Branches are `<type>/<name>`, matching the commit type: `fix/`, `feat/`,
  `ci/`, `docs/`, `chore/`, `test/`.
- A commit is a subject plus flat one-sentence bullets. Subjects are
  declarative, not imperative: "the run-end bound is checked", not "check the
  run-end bound".
- **No AI attribution and no co-author trailers**, in commits or in pull
  request descriptions.
- `main` takes **squash merges only**.

## Project rules

- **No GPL/LGPL/AGPL dependencies.** Permissive only (MIT/BSD/Apache).
  Shelling out to a copyleft CLI as a *test oracle* is fine — linking or
  copying it is not.
- **Each of these is a standalone project.** Never mention a consuming
  application in the README, the source, or CLI help.
<!-- END SHARED BLOCK: agent-core v2 -->

## Where the shared block does not map cleanly

- **"Output is budgeted"** names `scripts/tier.sh`. There is no `tier.sh`
  here: every chore task runs through `scripts/task.sh`, which hands the
  command to **rust-fs-core's** `scripts/output-budget.sh`, resolved at run
  time by `scripts/resolve-output-budget.sh` and deliberately **not committed
  here**. `chore siblings` puts `../rust-fs-core` at or above the pinned
  `FS_CORE_REF`; `FS_CORE_ROOT` overrides it and is authoritative. Without a
  core every task fails loudly rather than running unbudgeted. The measured
  budgets sit beside each command in `chores.yml`, and `tests/output-budget.sh`
  fails any task that has none — a new task needs a measured budget and a line
  in that test's task list.
- **"Validate against something that is not us"** means **real Windows**.
  The runner's unit tests and `tests/state-machine.sh` prove the harness agrees
  with itself, and nothing more. What the harness promises about Windows is
  checked by the `smoke` job on `windows-latest`: WinFsp's `memfs` stands in
  for a driver, the runner SSHes to itself, and the real `run-tests.sh` mounts,
  drives every op script on a drive letter, unmounts and ships the image back
  for the host verifiers. Its `canary-wrong-content` scenario **must** be
  reported failed — a harness that cannot go red proves nothing when it is
  green. `tests/chkdsk-verdict.ps1` pins how chkdsk's exit codes and reports
  become verdicts, which is the interpretation every consumer inherits; a
  change to it needs a fixture from a real chkdsk run, not one written to agree
  with the code.
- **"Claiming work"** is about GitHub issues. `docs/multi-agent-protocol.md`
  is a different claim: scenarios inside a consumer's `test-matrix.json`,
  through `claim-scenario.sh` and `FS_HARNESS_SESSION`. Do not mix them up.

## Running tests

```sh
chore siblings        # first: ../rust-fs-core, where the output budget comes from
chore check           # everything that runs off Windows (the list below)
chore lint            # bash -n, shellcheck, cargo fmt --check, clippy -D warnings
chore test            # runner unit tests
chore state-machine   # claim / update-status / reset, incl. concurrent writers
chore output-budget   # the wrapper resolves from core; every task is budgeted
chore agents-core     # AGENTS.md carries the shared block; drift is refused
chore config          # schemas, examples, negative fixtures (python3 3.11+, jsonschema)
chore smoke --vm-host user@vm --ssh-key ~/.ssh/vm --vm-workdir C:/fswth/smoke-consumer
```

Any task takes `-- --verbose` to stream the run; otherwise it prints one
verdict line and the log is in `tmp/logs/<task>.log`.

CI (`.github/workflows/ci.yml`), every job running the same task as above:

| job | runs |
|---|---|
| `lint (shell + cargo)` | `chore lint` |
| `runner unit tests` | `chore test` |
| `state-machine integration test` | `chore state-machine`, `chore output-budget`, `chore agents-core` |
| `config (schemas, examples, negative fixtures)` | `chore config` |
| `smoke (windows-latest, …)` | `tests/chkdsk-verdict.ps1`, `tests/vm-lock.ps1`, the VM-workdir lock contention check, `chore smoke` |
| `ci-ok` | the single required check: every job above ran and succeeded, and none is missing from its `needs` |

**Do not point `chore smoke` at a shared Windows VM from an agent** unless you
were asked to. It takes the VM-workdir lease for the whole run and mounts
volumes on the VM's drive letters; the `smoke` CI job exercises the same path
on a throwaway runner.

## Things a newcomer trips on

- **The harness stays filesystem-agnostic.** Knowledge of a particular
  filesystem — its mount binary, its ops, its verifier — belongs in the
  consumer's `fs-windows-test-harness.toml`, not in `scripts/` or `runner/`.
  A new capability is a new substitution token or op kind that any driver
  could use (`docs/vocabulary.md`).
- **Scenarios run one at a time.** Drive-letter assignment and WinFsp host
  state are process-global on Windows, so the runner serialises scenarios and
  forces `--test-threads=1`. Parallelism is separate VMs, never threads.
- **The host-side shell must run on macOS's bash 3.2**: no associative
  arrays, no `mapfile`. `tests/state-machine.sh` is written to that floor.
- **The VM-workdir lease is fenced.** A live `run-tests.sh` renews its lock;
  an expired one can be reclaimed, and an old owner cannot touch the VM or
  release its replacement. `FSWTH_VM_LEASE_SECONDS` and
  `FSWTH_VM_HEARTBEAT_SECONDS` tune it (heartbeat at most a third of the
  lease). Do not "simplify" the fencing away.
- **A schema change lands with a negative fixture.** `tests/config-fixtures/`
  holds configs that must be rejected, each with an `expect.txt` naming the
  reason; both the runner's loader and `tests/validate-configs.py` are held
  to them.
- **`.test-env` is per-machine** (VM host, key, workdir) and gitignored.
  Never commit one.

## Releasing

Consumers pin a tag, so a fix reaches them only once it is released. A pull
request adds a `## vX.Y.Z — <date>` section to `CHANGELOG.md`; once it is
merged and `ci-ok` is green on `main`, the merge commit is tagged `vX.Y.Z`.
There is no release workflow and no build artifact: the tag is the release.
Semver applies from `2.0.0`; an entry a consumer must act on (a renamed task,
a config key, a new VM requirement) says so in its heading, as `v4.2.0`'s does.
