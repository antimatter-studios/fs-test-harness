# fs-windows-test-harness

> Reusable Mac-orchestrator + Windows-VM-agent test harness for filesystem driver projects.

[![CI](https://github.com/antimatter-studios/fs-windows-test-harness/actions/workflows/ci.yml/badge.svg)](https://github.com/antimatter-studios/fs-windows-test-harness/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust 1.79+](https://img.shields.io/badge/rust-1.79%2B-orange.svg)](https://www.rust-lang.org)
[![Changelog](https://img.shields.io/badge/changelog-CHANGELOG.md-blue.svg)](./CHANGELOG.md)

## What is this

A drop-in test harness for people writing **filesystem drivers** on
macOS that have to run on Windows. You point a `harness.toml` at your
binary, write a `test-matrix.json` of scenarios, and the harness
takes care of the rest: tar your source to a Windows VM over SSH,
shell out to your driver, mount images, run ops on the mounted volume,
compare results, capture per-scenario diagnostics, and pull everything
back to your laptop.

The state machine is atomic — `claim` / `update-status` / `reset` over
JSON — so multiple agents can fan out across one matrix without
clobbering each other. Driver-specific knowledge stays in the
consumer's `harness.toml` (op templates, mount command, ready-line
regex), so this repo stays filesystem-agnostic. Today it backs
[ext4-win-driver](https://github.com/antimatter-studios/ext4-win-driver);
the same shape worked verbatim for an NTFS prototype before it.

## At a glance

| Path | What lives there |
| --- | --- |
| `runner/` | Rust crate. `lib.rs` exposes the `Harness` / `Adapter` API; `bin/run-matrix.rs` is the [libtest-mimic](https://github.com/LukasKalbertodt/libtest-mimic) driver. |
| `scripts/` | Mac-side bash + Windows-side PowerShell. `run-tests.sh` (single entrypoint — bootstrap, preflight, ship, run, diag-pull), plus state-machine helpers `claim-scenario.sh`, `update-scenario-status.sh`, `reset-non-passed.sh`, and `run-scenario.ps1` (the VM-side per-scenario executor). |
| `schemas/` | JSON Schema for `harness.toml` and `test-matrix.json`. |
| `docs/` | Long-form: consumer integration, architecture, triage, multi-agent protocol. |
| `examples/minimal/` | Smallest viable consumer config. |
| `tests/` | Self-tests: the state-machine test, config validation with negative fixtures (`config-fixtures/`), and `smoke-consumer/`, an end-to-end consumer that mounts volumes through WinFsp's memfs. |
| `.github/workflows/` | CI: lint, runner unit tests, state-machine, config validation, the Windows smoke test, and the aggregate `ci-ok` check. See [CI and automated merging](#ci-and-automated-merging). |

## Quickstart

For a new consumer. First, a Windows VM the Mac can reach over SSH at an
address that does not move: [`docs/vm-setup.md`](./docs/vm-setup.md) takes it
from an empty hypervisor to `ssh <user>@<vm> "echo ok"`.

```sh
# 1. Check the harness out AS A SIBLING of your repo, not inside it.
#    Consumers pin the ref in chores.yml and let `chore siblings` do
#    this, so every repo on the machine shares one copy rather than
#    each carrying its own -- which is how they used to end up on
#    different versions with nothing reporting it.
git clone https://github.com/antimatter-studios/fs-windows-test-harness.git ../fs-windows-test-harness

# 2. Drop a fs-windows-test-harness.toml + test-matrix.json next to
#    your Cargo.toml.
cp ../fs-windows-test-harness/examples/minimal/fs-windows-test-harness.toml ./fs-windows-test-harness.toml
cp ../fs-windows-test-harness/examples/minimal/test-matrix.json ./test-matrix.json
$EDITOR fs-windows-test-harness.toml   # point [project.binary] at your driver, fill [vm.*]

# 3. Run the matrix. On first run, prompts for VM details + writes
#    .test-env; subsequent runs skip straight to the matrix. See
#    `bash ../fs-windows-test-harness/scripts/run-tests.sh --help` for the full
#    surface.
bash ../fs-windows-test-harness/scripts/run-tests.sh
```

Setting up the Windows VM: [`docs/vm-setup.md`](./docs/vm-setup.md).
Persistent QEMU VM, one setup for Linux ARM64/KVM (including Raspberry Pi 5)
and Apple Silicon macOS/HVF: [`docs/qemu-vm.md`](./docs/qemu-vm.md). The guest
stays running between tests; CI continues to use its Windows runner directly.
Full contract: [`docs/consumer-integration.md`](./docs/consumer-integration.md).
Architecture overview: [`docs/architecture.md`](./docs/architecture.md).
Diagnosing a red scenario: [`docs/triage-protocol.md`](./docs/triage-protocol.md).
Concurrent-agent rules: [`docs/multi-agent-protocol.md`](./docs/multi-agent-protocol.md).
Substitution vocabulary + cross-driver naming convention: [`docs/vocabulary.md`](./docs/vocabulary.md).

## Self-test

Every CI job has a local equivalent in [`chores.yml`](./chores.yml)
(run with [chore](https://github.com/antimatter-studios/chore)); CI runs
the same commands.

```sh
chore siblings # first: put ../rust-fs-core where the output budget is read from
chore check    # lint + test + state-machine + remote-timeout + output-budget + agents-core + config: everything off Windows
chore lint     # bash -n, shellcheck, cargo fmt --check, cargo clippy
chore test     # runner unit tests
chore state-machine
chore output-budget  # the wrapper resolves from core, and every task is budgeted
chore agents-core    # AGENTS.md carries the shared agent-core block, unmodified
chore config   # needs python3 3.11+ and `pip install 'jsonschema>=4.19'`
chore local-vm # QEMU control checks; no guest or image download required
chore test -- --verbose   # any task: stream the whole run, not just the verdict

# End to end against a Windows host with WinFsp and sshd (PowerShell as
# the default shell). Flags are remembered in tests/smoke-consumer/.test-env.
chore smoke --vm-host user@vm --ssh-key ~/.ssh/vm --vm-workdir C:/fswth/smoke-consumer
```

## Output: quiet by default, `--verbose` on request

A task prints a **verdict**, not a transcript: a line saying it passed, how
much it printed, and the path of the log that holds everything else. The
detail is never thrown away — it is written to a log under the repository's
own `tmp/logs/` and named in that line — but a passing run does not put it on
the terminal.

| | Prints |
| --- | --- |
| **Pass** | `test: ok (52 lines, 2870 bytes) — …/tmp/logs/test.log` |
| **Fail** | the same label with `FAILED (exit N)`, the log and how many lines are in it — and no tail, unless `OUTPUT_BUDGET_FAIL_TAIL=N` asks for one |
| **`--verbose` / `-v`, or `OUTPUT_BUDGET_VERBOSE=1`** | everything, streamed live: every test name, every matrix step, every guest command |
| **CI** | the quiet form, with the log uploaded as an artefact |

Two reasons, and the second is the one people forget. A run that prints
three thousand lines hides the twenty that matter, so a failure costs
minutes of scrolling. And every reader pays for that output — a person, a CI
log viewer, and an agent working on the repository, which re-reads its whole
transcript on each step and so pays for a verbose run many times over.

For a consumer the loud part is the **matrix**: a line per recipe step per
scenario, and chkdsk's report for every image it checks. That is exactly what
someone needs when a scenario fails, so it goes to the log — never to
`/dev/null` — and the tail of it is what a failure prints.

The wrapper that does this is **[rust-fs-core](https://github.com/antimatter-studios/rust-fs-core)'s
`scripts/output-budget.sh`**, and it is deliberately **not committed here**.
It runs a command, writes everything to `--log`, prints one line on success,
prints a verdict naming the log on failure, and **exits 65 when a run passed
but printed more than its budget** — a status you can tell apart from a
failing suite. `--verbose` (or `OUTPUT_BUDGET_VERBOSE=1`) streams as well, and
does not exempt a run from its budget. The command's own exit status is what
escapes: `cmd | tee log` reports tee's, which is how a red suite reads green.

A consumer of this harness calls core's copy, not this repository's:

```sh
../rust-fs-core/scripts/output-budget.sh \
    --log tmp/logs/matrix.log --max-lines 600 --max-bytes 60000 --label 'test:matrix' \
    -- scripts/run-matrix.sh
```

Set a budget from a measured run, and raise it deliberately when a suite
grows — the same way an executed-test floor is set. A budget nobody can
breach measures nothing. This repository's own tasks run through
[`scripts/task.sh`](./scripts/task.sh), with the measured budgets beside each
command in [`chores.yml`](./chores.yml), and
[`tests/output-budget.sh`](./tests/output-budget.sh) fails any task that has
none.

### Where the wrapper comes from

One script, in one repository. This harness kept a copy of it until
rust-fs-core#153, as did fs-linux-test-harness and several drivers, and the
copies drifted: this one read `FWTH_VERBOSE`, the Linux harness's read
`FLTH_VERBOSE`, and the canonical one reads `OUTPUT_BUDGET_VERBOSE` and
reports either of the old names rather than silently ignoring it. Each
repository was internally consistent and nothing compared them.

So `scripts/output-budget.sh` is gone from here, and
[`scripts/resolve-output-budget.sh`](./scripts/resolve-output-budget.sh)
finds core's at run time — `$FS_CORE_ROOT` if it is set (authoritative, no
fallback), otherwise a `rust-fs-core` sibling beside the main working tree.
It verifies `--version` rather than a digest, because a digest pinned in
several repositories is the lockstep this removes. A core that is missing, or
present and answering a different API version, **fails the task loudly**
rather than letting it run unbudgeted.

`chore siblings` clones or advances that sibling; the pin, `FS_CORE_REF`, is
in [`chores.yml`](./chores.yml) and is a floor, not a target — it never moves
a shared core checkout backwards. CI has no sibling directory, so it clones
core into the runner's temp directory and exports `FS_CORE_ROOT`.

```sh
chore siblings          # put ../rust-fs-core at or above the pinned ref
```

CI runs every task through the same budgeted wrapper, including the Windows
`smoke` task. The full logs are uploaded as workflow artifacts under
`tmp/logs/`; the smoke artifact also contains the pulled scenario and VM
diagnostics. A green job therefore stays quiet while its complete evidence is
still available for inspection.

## CI and automated merging

A green CI run is meant to be enough to merge on, with no human looking
at it. Each job proves one thing:

| Job | Proves |
| --- | --- |
| `lint (shell + cargo)` | Every shell script parses and passes shellcheck (errors); the runner is `rustfmt`-clean and `clippy -D warnings`-clean. |
| `runner unit tests` | The runner's substitution, dispatch, config loading, `.test-env` parsing and disk hygiene behave, and its loader accepts every consumer config in the repo and rejects every fixture in `tests/config-fixtures/invalid/`. |
| `state-machine integration test` | `claim` / `update-status` / `reset` transition statuses correctly, and concurrent claimers and writers neither double-claim nor lose updates. The remote-timeout, output-budget and agent-guide self-tests run here too. |
| `config (schemas, examples, negative fixtures)` | Both schemas are valid; every consumer config (`examples/*`, `tests/smoke-consumer`) validates and only uses declared ops; every negative fixture is rejected for the reason its `expect.txt` names. |
| `smoke (windows-latest, WinFsp memfs, run-tests.sh over SSH)` | The harness works end to end on real Windows. See below. |
| `ci-ok` | Every job above succeeded (not failed, cancelled or skipped), and no job exists that `ci-ok` does not wait for. |

**`ci-ok` is the single required status check** to configure in branch
protection / auto-merge. Jobs can be added, renamed or removed without
touching repository settings: `ci-ok` fails if its `needs:` list and the
workflow's jobs drift apart. CI runs on every pull request and on pushes
to `main`; a newer push to a pull request cancels the older run.

### The smoke test

The `smoke` job installs WinFsp on `windows-latest` and uses **WinFsp's
sample `memfs` filesystem as a stand-in driver**. The runner is its own
"VM": the job enables key-based SSH to `localhost` with PowerShell as the
default shell, then runs the real `scripts/run-tests.sh` against
[`tests/smoke-consumer`](./tests/smoke-consumer), an ordinary consumer
whose volumes are tar images:

1. **bootstrap and preflight** -- `.test-env` is written from flags, and
   the SSH preflight trusts the new host key itself;
2. **exclusive VM-workdir lease** -- a live `run-tests.sh` renews its lock
   while it runs; an expired lock can be reclaimed, and an old owner cannot
   mutate the VM or release its replacement. CI exercises contention,
   recovery, fencing, and a short lease in the end-to-end smoke run;
3. **ship** -- harness and consumer VM scripts go to the VM workdir;
4. **`smoke-rw-roundtrip`** -- the host creates an image and ships it to
   the VM; each VM step mounts it through memfs on a drive letter
   (ready-line matched), runs one of the harness's op scripts (list, read
   with content/size/sha256 checks, mkdir, write, rename, unlink, rmdir)
   and unmounts; later steps only see earlier changes if they survived
   the unmount. The image is shipped back and the harness's host
   verifiers (`scripts/host/verify-ls.sh`, `verify-cat.sh`) check it.
   Every scenario must pass on its first attempt, with `result.json`,
   `recipe.json`, `results.json`, `run-manifest.json` and each step's
   `step.json` / `stdout.txt` / `stderr.txt` present, and each op's output
   proving it acted on the drive;
5. **`canary-wrong-content`** -- expected to fail: it reads a file back
   with the wrong expected content. `run-tests.sh` must exit non-zero and
   the harness must report `failed` at that step, with the verifier's
   `content mismatch`. A harness that cannot go red proves nothing when
   it is green.

memfs keeps volumes in memory, so
[`memfs-mount.ps1`](./tests/smoke-consumer/scripts/fs-windows-test-harness/memfs-mount.ps1)
loads the tar image into the drive on mount and writes changes back to
it while mounted -- the image I/O a real driver does itself.

The VM lease defaults to 30 minutes with a 60-second heartbeat. The full
matrix takes 3–4 hours, while a normal scenario is about 5–8 minutes; the
lease leaves room for a slow step or a brief SSH outage without blocking
crash recovery for the entire matrix duration. Set
`FSWTH_VM_LEASE_SECONDS` and `FSWTH_VM_HEARTBEAT_SECONDS` to tune it; the
heartbeat must be at most one third of the lease. The PID in diagnostics is
the orchestrator's PID and is never used as a Windows liveness check.

Every command sent to the VM is bounded, so a session that stops answering
is a failure that names itself rather than a run that hangs until CI cancels
it. `run-tests.sh` stops a lock action, ship or scp after
`FSWTH_REMOTE_TIMEOUT_SECONDS` (300), `--reinstall` and
`[run].vm_build_command` after `FSWTH_VM_SETUP_TIMEOUT_SECONDS` (3600), and
the runner stops any recipe step after `FSWTH_STEP_TIMEOUT_SECONDS` (900).
Each prints the command, the bound and the scenario:

```
[run-tests] TIMEOUT: VM lock Acquire (ssh runneradmin@localhost) did not finish within 300s (scenario filter 'mac-format-mkdir-rmdir-win-chkdsk'); stopped it
[12:04:31][+00:15:02] FAIL mac-format-mkdir-rmdir-win-chkdsk  (total 00:15:02)  — step 5 (win-chkdsk) errored: timed out after 900s (FSWTH_STEP_TIMEOUT_SECONDS) and was stopped: ssh runneradmin@localhost ...
```

The runner also prints a line as each step starts, naming its scenario, so
the last `start` in a log is the step that was running.

## License

[MIT](./LICENSE). Pure orchestration code — no copyleft input, no
copyleft propagation. Consumer projects that themselves link copyleft
components (e.g. WinFsp Rust bindings under GPL-3.0) re-license their
own binary as required; the harness stays permissive.

## Provenance

Originally extracted from two near-identical copies that lived inside
[`rust-fs-ntfs`](https://github.com/antimatter-studios/rust-fs-ntfs)
and
[`ext4-win-driver`](https://github.com/antimatter-studios/ext4-win-driver).
Both followed the same shape — build a reference image, exercise our
driver against it, post-verify with a structural checker — so the
shape was lifted into this repo and the FS-specific bits pushed into
`harness.toml`. The harness itself doesn't know or care which
filesystem it's testing.

Releases and breaking changes are recorded in
[`CHANGELOG.md`](./CHANGELOG.md); semver applies from `2.0.0` onward.
