# Changelog

All notable changes to fs-windows-test-harness will land here. The format
loosely follows Keep a Changelog; semver applies from `2.0.0` onward.

## [Unreleased]

### Fixed

- **A recipe reference inside a resolved value is expanded, and a missing one
  stops the step (#45).** A step value such as `"{scenario.volume_params.label}"`
  reached Windows as that literal text, because placeholders were expanded in
  one pass; a required reference that resolved to nothing became an empty
  string. References now expand through scenario and step values, up to eight
  deep, with a cycle or a deeper chain reported; every unresolved required
  reference fails the step before its command runs, naming each one; and
  `{{`, `}}` and `{"literal": ...}` values stay literal.

### Changed

- **The agents-core check runs in place from rust-fs-core.** The copy in
  `scripts/agents-core-check.sh` is gone; `tests/agents-core.sh` runs core's
  from `$FS_CORE_ROOT` (else `../rust-fs-core`), and the pinned core moves
  from v0.2.13 to v0.3.3, the release whose check reads the guide of the
  repository it is run for (rust-fs-core#212).

## v4.3.1 — 2026-10-03

### Fixed

- **A host-side op's double quotes reach it intact on a Windows
  orchestrator.** The runner passed the command to `cmd.exe /C` as an
  ordinary argument, so Rust escaped every `"` as `\"` for the C runtime's
  parser. `cmd.exe` does not parse arguments that way and hands the line on
  verbatim, so a script received the backslashes too: a needle written as
  `'label:          "testvolume"'` arrived as `'label:          \"testvolume\"'`
  and never matched. The command now goes to `cmd.exe /S /C "<command>"`
  unescaped. The smoke consumer's last step proves it on `windows-latest`
  with `verify-info.sh` and a double-quoted, space-padded needle.

## v4.3.0 — 2026-10-02

### Fixed

- **A VM command that never returns is a named, bounded failure, not a
  silent hang.** On CI, one SSH session to the Windows runner stopped
  answering three times and the job printed nothing until it was cancelled,
  40 minutes or six hours later (rust-fs-ntfs#403, #388). Every command
  `run-tests.sh` sends to the VM — preflight, each VM-lock action, ship, scp,
  `--reinstall`, `[run].vm_build_command` — now runs through
  `harness_bounded`, and every recipe step the runner dispatches (ssh, scp,
  host command) through `output_with_timeout`. On timeout the command is
  stopped and the failure names it, the bound and the scenario:
  `[run-tests] TIMEOUT: VM lock Acquire (ssh …) did not finish within 300s
  (scenario filter '…')`, or a scenario FAIL line ending `step 5 (win-chkdsk)
  errored: timed out after 900s (FSWTH_STEP_TIMEOUT_SECONDS) and was
  stopped: ssh … <command>`.
- **The VM-lock helper is no longer streamed on the remote stdin.** All three
  recorded hangs stopped inside a lock call that piped
  `matrix-run-lock.ps1` into `[Console]::In.ReadToEnd()` on the VM (Acquire,
  the Verify before the ship, the Release at the end), while the far more
  numerous guarded commands, which never read stdin, never hung. The helper
  now travels gzipped inside the encoded command (about 12,300 characters)
  and lock calls run `ssh -n`, so nothing waits on an end-of-file Windows
  OpenSSH may not deliver.

### Added

- **Three bounds, each an environment variable:**
  `FSWTH_REMOTE_TIMEOUT_SECONDS` (default 300) for lock actions, preflight,
  ship and scp; `FSWTH_VM_SETUP_TIMEOUT_SECONDS` (3600) for `--reinstall`
  and `[run].vm_build_command`; `FSWTH_STEP_TIMEOUT_SECONDS` (900) for each
  recipe step. A value that is not whole seconds in 1..86400 stops the run
  before it starts.
- **A line as each recipe step starts,** naming the step and its scenario
  (`  05 win-chkdsk           start (<scenario>)`), so the last `start` in a
  log is the step that was running. A consumer whose output budget counts
  the runner's lines gains one line per step executed.
- **`chore remote-timeout`,** which runs `tests/remote-timeout.sh`: a
  stand-in `ssh` that never returns from Acquire, Verify or Release, and the
  run must fail within its bound naming the command and the scenario, with
  no stand-in left running. CI runs it in the `state-machine` job.
- **`run_recipe_observed`, `output_with_timeout` and `step_timeout`** in the
  runner library, for the start callback and the bound.
- **An agent guide, `AGENTS.md`, with `CLAUDE.md` importing it.** It carries
  the agent-core block shared byte-identically across the repositories this
  harness is developed alongside, then what is specific to this one: where the
  oracle is (the Windows smoke job and its canary), how the tasks and budgets
  run, and what a newcomer trips on.
- **`chore agents-core`,** which runs `tests/agents-core.sh`: the committed
  guide passes `scripts/agents-core-check.sh`, and a modified, unmarked,
  mis-declared or absent one is refused. CI runs it in the `state-machine` job.

## v4.2.0 — 2026-09-26

### Changed (BREAKING for anyone calling the harness's wrapper directly)

- **The output-budget wrapper comes from `rust-fs-core` and is no longer
  committed here.** `scripts/output-budget.sh` is deleted;
  `scripts/resolve-output-budget.sh` finds core's copy at run time —
  `$FS_CORE_ROOT` if set (authoritative, no fallback), otherwise a
  `rust-fs-core` sibling beside the main working tree — and verifies its
  `--version`, not a digest. A missing core, or one answering another API
  version, fails the task loudly instead of letting a suite run unbudgeted.
  `chore siblings` puts core where the resolver looks; CI clones it at the
  pinned `FS_CORE_REF` (`v0.2.13`) and exports `FS_CORE_ROOT`.

  Three consequences for anyone driving tasks here. The verbose variable is
  now **`OUTPUT_BUDGET_VERBOSE`**, not `FWTH_VERBOSE`; the canonical script
  reports the old name on stderr rather than honouring it. **A failing task
  prints no tail by default** — it names the log, its status and its line
  count; `OUTPUT_BUDGET_FAIL_TAIL=40` restores the old behaviour. And a
  consumer that called `../fs-windows-test-harness/scripts/output-budget.sh`
  calls `../rust-fs-core/scripts/output-budget.sh` instead.

  Why: the copies had already drifted. This one read `FWTH_VERBOSE`,
  fs-linux-test-harness's read `FLTH_VERBOSE`, core's reads
  `OUTPUT_BUDGET_VERBOSE`, and this one printed forty lines of tail where
  core prints none — each repository internally consistent, and nothing
  comparing them (rust-fs-core#153).

### Added

- **`chore siblings`,** which clones or advances `../rust-fs-core`. The pin is
  a floor, not a target: a checkout already at or ahead of it is left alone,
  and one below it is only moved by fast-forwarding a clean `main`. One core
  checkout is shared by every repository on a machine, and a task that checks
  out its own pin rewinds somebody else's work.
- **`chore output-budget`,** the local equivalent of the CI step that was only
  reachable through the workflow. `tests/output-budget.sh` now drives both
  refusals — core absent, and core present but answering the wrong API
  version — proves a task with no resolvable wrapper does not run its command
  at all, and keeps the rule that every task carries two non-zero budgets. It
  no longer re-tests the wrapper's own behaviour: that suite lives in
  rust-fs-core, where the script does.
- **Per-scenario mutual exclusion with `exclusive_group`.** Scenarios that
  name the same scarce resource still share the runner's global parallelism
  limit, but never execute together. This lets consumers serialize only
  disk- or memory-heavy cases instead of slowing the entire matrix.

### Fixed

- **A long VM command no longer fails the scenarios running beside it.**
  Every VM command of a run held the workdir's operation gate exclusively,
  so parallel scenarios queued on it, and any command longer than the gate's
  15-second wait (a chkdsk, say) failed its siblings' next VM step. The
  owning run's commands now share the gate. Acquire and release still take
  it exclusively. (#34)
- **Two matrix processes can no longer share one VM workdir.**
  `run-tests.sh` atomically claims a lock directory after SSH preflight and
  before reinstall, ship, or scenario execution. Contention fails fast with
  the owning run, host, and PID. A heartbeat keeps a live run's lease fresh;
  an expired lock is recovered under a Windows file-share gate. Every VM
  command and final image move is fenced by the owner token, and cleanup
  cannot release a replacement owner's lock. This protects flat image names
  and broad image cleanup across crashed and concurrent runs.

### Added

- **`docs/vm-setup.md`: setting up the Windows VM**, from an empty
  hypervisor to SSH answering. It covers a host-only network with a static
  guest address (the NAT address moves, and `known_hosts` and `VM_HOST`
  break with it); the Private firewall profile that address needs before
  sshd will answer on it; OpenSSH server and where an administrator's key
  goes; VMware Fusion's encrypted VM and `vmrun -vp`; running headless;
  and what a VM that is catching up on Windows Update looks like. Linked
  from the README Quickstart and from `docs/consumer-integration.md`.

### Changed

- **A scenario must pass on its first attempt.** The runner no longer retries
  a failed scenario up to five times and turns a later pass green. Transient
  infrastructure and resource failures are now visible failures with their
  first diagnostics intact.
- **CI now enforces the output budgets it documents.** Lint, runner tests,
  state-machine, config validation, and the Windows smoke task all run
  through `scripts/task.sh`; smoke has a measured 500-line / 60,000-byte
  budget. Every job uploads its full log, and the smoke job uploads its
  scenario and VM diagnostics instead of dumping them into the workflow log.

### Removed

- **The in-tree `.githooks/pre-commit` and `scripts/install-hooks.sh`.**
  github-guard's hooks, installed into `.git/hooks`, now run fmt and clippy
  for this repository's `runner/` crate (antimatter-studios/agent-skills#57).
  Before that they only looked for a `Cargo.toml` at the repository root.
  The in-tree hook also needed `core.hooksPath = .githooks`, which switched
  off every github-guard guard in the clone. A clone installs the hooks with
  github-guard's `install.sh`, which also clears `core.hooksPath`.

## v4.1.0 — 2026-09-18

### Added

- **`scripts/output-budget.sh`: a task prints a verdict, `--verbose` prints
  the run, and a budget can fail the build.** It runs a command, writes the
  whole run to a log, prints one line on success and the log's tail on
  failure, and exits 65 when a run passed but printed more than its line or
  byte budget. `--verbose` or `FWTH_VERBOSE=1` streams as well, and is still
  budgeted. The same script as fs-linux-test-harness's, so consumers of
  either harness budget their test tiers the same way -- the matrix run and
  chkdsk output go to a log, not the terminal. See the README, "Output: quiet
  by default".
- This repository's own tasks (`lint`, `test`, `state-machine`, `config`)
  now run that way through `scripts/task.sh`, each under a measured budget,
  and `tests/output-budget.sh` fails a task added without one.

## v4.0.0 — 2026-09-18

Breaking. Consumers rename in lockstep; see below.

### Changed (BREAKING)

- **Renamed from `fs-test-harness` to `fs-windows-test-harness`.** The
  harness is filesystem-agnostic but Windows-specific (Mac orchestrator
  → Windows VM over SSH, PowerShell executor, WinFsp drive-letter
  mounts), and the name now says so. The repo is
  `antimatter-studios/fs-windows-test-harness`; the runner crate is
  `fs-windows-test-harness` (lib `fs_windows_test_harness`; the
  `run-matrix` bin is unchanged).
- **The old names are no longer read; there is no fallback.** Consumers
  must rename in lockstep with bumping to this release:
  - config file `fs-test-harness.toml` → `fs-windows-test-harness.toml`
    (`HARNESS_TOML` still overrides the path);
  - default `[vm] scripts_dir` `scripts/fs-test-harness` →
    `scripts/fs-windows-test-harness` (or declare `scripts_dir`
    explicitly);
  - sibling checkout `../fs-test-harness` → `../fs-windows-test-harness`,
    and clone URLs → `https://github.com/antimatter-studios/fs-windows-test-harness`.
- **`{vm.harness_root}` defaults to the sibling checkout**
  `{vm.workdir}/../fs-windows-test-harness` instead of the dead
  `vendor/fs-test-harness` submodule path. `HARNESS_DIR` in `.test-env`
  still overrides it.

### Added

- **Windows smoke test in CI.** A `smoke` job on `windows-latest`
  installs WinFsp and runs the real `scripts/run-tests.sh` against the
  runner itself over SSH to `localhost`, with WinFsp's sample `memfs`
  as the filesystem driver (`tests/smoke-consumer`). It mounts,
  writes, mkdirs, renames, unlinks, rmdirs, reads back with
  content/size/sha256 checks, lists, ships the image back for the host
  verifiers, and asserts the verdicts and diagnostics -- plus a canary
  scenario that must be reported `failed`. Replaces the
  `mock-scenario` job and the `tests/mock-fs` crate, which mounted
  nothing.
- **Config tests.** `tests/validate-configs.py` (CI job `config`)
  validates every consumer config in the repo against the schemas and
  checks recipes only use declared ops; negative fixtures under
  `tests/config-fixtures/` must be rejected, by the schemas and (for
  `invalid/`) by the runner's loader too.
- **`ci-ok`**, one aggregate check that fails unless every other job
  succeeded -- the single status check for branch protection and
  auto-merge. CI now runs on pull requests and pushes to `main`, and a
  newer push cancels a pull request's older run.
- **`chores.yml`**: `chore check` (lint, test, state-machine, config)
  and `chore smoke` (the smoke test against a Windows host) reproduce
  CI locally.

### Fixed

- **Concurrent `claim-scenario.sh` could hand one scenario to two
  agents, and concurrent status writes could be lost.** Writers read,
  modified and renamed the matrix with no mutual exclusion; the
  read-back check meant to detect a lost race was itself unserialised.
  All three state-machine scripts now run their read-modify-write under
  an exclusive `flock` (`scripts/_matrix_txn.py`). Showed up as
  `tests/state-machine.sh` section [4] failing about one run in five;
  measured 11/30 before (and a new lost-update test, [4b], 30/30),
  0/100 after.
- **The runner could not read the `.test-env` that `run-tests.sh`
  writes.** Lines are written as `export VM_HOST="..."`; the runner
  took `export VM_HOST` as the key and kept the quotes, so
  `${VM_HOST}` / `${SSH_KEY}` / `${VM_WORKDIR}` in the config expanded
  to nothing and every vm-step failed with "requires [vm].host". A
  leading `export` and one pair of matching quotes are now stripped.
- **`run-tests.sh` now ships the harness's `scripts/vm/` to
  `{vm.harness_root}`**, as its `--help` already said it did. Before,
  the VM ran whatever copy of the op scripts had last been put there by
  hand, or failed when there was none.
- **A relative `HARNESS_TOML` or `CONSUMER_ROOT` named different files in the
  two halves of a run.** `run-tests.sh` resolved the path from the directory it
  was invoked in, then changed into the consumer root and exported the same
  relative path for the Rust runner, which resolved it from there — so the
  shell and the runner could read different configs, or the runner none.
  `_lib_harness.sh` now makes both absolute as it resolves them; a Windows
  drive path is left as it is.
- **Host verifiers work without `shasum`.** `scripts/host/verify-*.sh`
  use `sha256sum` when present and fall back to `shasum -a 256`; Git for
  Windows ships only the former, so any `--expect-sha256` check failed
  with "command not found" on a Windows orchestrator. `scripts/host/` is
  now linted in CI.
- The state-machine scripts no longer leave the matrix file mode
  `0600` (a side effect of `mktemp`).

## [3.11.0] — 2026-06-02

### Added

- **Per-scenario host-image cleanup.** The runner now deletes each
  scenario's staged `{image_dir}/{run_id}/nfs-*.img` as soon as that
  scenario's recipe finishes, so a full matrix never holds every image
  at once (previously they accumulated until the end-of-run cleanup).
  `HARNESS_KEEP_IMAGES` is now value-aware — `0` / `false` / `no` /
  `off` / empty keep nothing extra, any other value preserves images —
  rather than presence-only. (#13)

### Fixed

- **Orphaned image directories from killed runs are now reclaimed
  automatically.** Each run stamps `{image_dir}/{run_id}/owner.pid`
  with its process id, and at startup the runner reaps any run dir
  whose owner pid is no longer alive. A run killed by SIGKILL, crash,
  or cancellation — none of which can run a cleanup handler — is
  reclaimed by the very next run instead of leaking tens of GiB of
  staged images that pile up across runs until the disk fills. Safe by
  construction: only numeric `run_id` dirs are considered (foreign
  content under a shared image dir such as `/tmp` is untouched), a live
  owner is never reaped (`kill -0` liveness, so concurrent runs are
  safe), and an unmarked legacy dir is removed only when it both
  carries the `nfs-*.img` staging signature and exceeds a 5-minute
  grace period. (#14)

## [3.10.0] — 2026-05-28

### Added

- **`verify-getxattr.sh`** — host-op script that calls the consumer
  binary's `getxattr <image> <path> <name>` subcommand and asserts
  stdout against `--expect-size`, `--expect-sha256`, and/or
  `--expect-content`. Filesystem-agnostic.

- **`verify-readlink.sh`** — host-op script that calls the consumer
  binary's `readlink <image> <path>` subcommand and compares the
  trimmed output to `--expect-target`. Filesystem-agnostic.

- **`harness.toml [vm.packages]` table-form entries** — each entry
  is now either a bare winget ID string (existing behaviour) OR an
  object `{ id = "...", custom_args = "..." }`. The `custom_args`
  string is forwarded to `winget install --override "<args>"`.
  Closes the fresh-VM gap for consumers whose packages need
  non-default installer features (e.g. `WinFsp.WinFsp` +
  `ADDLOCAL=F.Main,F.User,F.Developer` for bindgen consumers).

- **`setup-windows-vm.ps1 -PackagesJson`** — new parameter accepting
  the resolved `[vm.packages]` spec as a JSON-array string. Merged
  with the legacy `-ExtraPackages`; JSON entries win on duplicate
  IDs.

----

## [3.9.0] — 2026-05-27

### Added

- **Named scenario groups.** `[scenarios.<group>]` tables in
  `fs-test-harness.toml` let related scenarios share a label; the runner
  expands group names in filter expressions and prints the group in PASS/FAIL
  lines.

- **`.test-env` variable expansion in scenario config.** Values like
  `${VM_WORKDIR}` in scenario tables are now substituted from `.test-env`
  before the scenario runs.

- **`{scenario_name}` substitution in flat vocabulary.** The scenario's own
  name is now available as `{scenario_name}` in path/command templates.

### Fixed

- **`ship-to-host` destination parent created automatically.** `mkdir -p` is
  now run on the destination parent before `scp`, preventing failures when
  the target directory doesn't exist yet.

- **`SSH_OPTS` built from `SSH_KEY`.** The harness now constructs
  `SSH_OPTS="-i <key> -o IdentitiesOnly=yes"` automatically when `SSH_KEY`
  is set in `.test-env`, so consumers no longer need to duplicate key path
  in both variables.  Group-filter expansion clears the expanded list before
  each pass (prevents duplicate scenario runs on repeated filters).

----

## [3.7.0] — 2026-05-25

### Added

- **Retry-aware work queue with adaptive concurrency.** Scenarios that
  fail with "resource exhaustion" (VM drive-letter lock timeout) are
  pushed to the *back* of a shared `VecDeque`, so smaller tests run
  first instead of queuing behind a stuck large one. Concurrency is
  reduced by one on each exhaustion event (floor = `max(1, in_use)`);
  a probe thread restores one slot every 300 s of quiet so the runner
  doesn't stay clamped at 1 forever. `MAX_RETRIES = 5` before a
  scenario is permanently failed.

- **File-based drive-letter mutex on the VM** (`$env:TEMP\vhd-mount.lock`).
  `Acquire-DriveLock` in `_lib.ps1` uses `FileMode.CreateNew` (atomic on
  NTFS) to serialise the snapshot→mount→confirm window across concurrent
  SSH sessions. Retries 6 × 30 s (3 min total); clears stale locks older
  than 60 s. Lock file records holder + PID for diagnostics. Throws
  "resource exhaustion" on timeout — caught by the Rust retry queue.

### Changed

- **`serialize_mounts` removed; `max_parallel` default is now `1`
  (sequential).** `serialize_mounts = true` was equivalent to
  `max_parallel = 1` — the flag is gone. Consumers that had
  `serialize_mounts = false` should remove the line; the old
  `max_parallel = "drive-letters"` / integer form is unchanged.
  `run-tests.sh` derives `--test-threads` from `max_parallel`
  automatically (no separate `test_threads` key needed).
  Clamped to `1..=24` in the runner (Windows has 26 drive letters;
  A and B are reserved).

----

### Added

- **`[vm.packages]` per-package custom installer args.** Entries
  can be either a bare `"PkgId"` string or a table `{ id = "PkgId",
  custom_args = "..." }`. `custom_args` is forwarded to the
  underlying installer via winget's `--override` flag. Motivating
  case: WinFsp's MSI ships with `F.Developer` (headers + .lib) off
  by default, which breaks `bindgen` for consumers that build
  WinFsp bindings on the VM. Declaring `{ id = "WinFsp.WinFsp",
  custom_args = "ADDLOCAL=F.Main,F.User,F.Developer" }` includes
  the dev pack. Wired through:
    - `harness.schema.json` accepts both forms via `oneOf`.
    - `runner/src/config.rs` deserialises into `Vec<PackageSpec>`
      (untagged enum: `Bare(String)` or
      `Table { id, custom_args }`).
    - `setup-windows-vm.ps1`'s `-ExtraPackages` accepts mixed
      string + hashtable entries; `Resolve-PackageEntry` normalises
      them.

- **`setup-windows-vm.ps1 -Reinstall`** switch. Uninstall-then-
  install every consumer package — winget reconfigure with new
  ADDLOCAL features against an already-installed MSI returns 1603
  / "feature not found"; uninstall+install always works. Includes
  a prelude that kills any hung `winget` /  `AppInstaller*` /
  `WinGetServer*` / `DesktopAppInstaller*` processes from previous
  SSH-driven runs and wipes `%LOCALAPPDATA%\Temp\WinGet\` to
  release file locks — without this, the second `--reinstall`
  invocation hangs indefinitely behind zombies from the first.
  Rustup is exempted from the uninstall step (its custom installer
  hangs over SSH); rustup is installed via direct download from
  `https://win.rustup.rs/<arch>` rather than through winget.

- **`run-tests.sh --reinstall`** flag. Generates a wrapper.ps1
  locally with `[vm.packages]` / `[vm.rust_toolchain]` /
  `[vm.workdir]` from `harness.toml` baked in, scp's both the
  wrapper and `setup-windows-vm.ps1` to the VM, ssh-invokes
  `powershell -File` against the wrapper. Continues into the
  normal ship + run flow on success. Combine with `--no-ship` for
  bootstrap-only.

### Notes

Backward-compatible: existing bare-string `packages =
["WinFsp.WinFsp", "LLVM.LLVM"]` entries keep working unchanged.
Object form is opt-in.

----

Pre-3.5.0 (was [Unreleased] on PR #9 merge — kept here because v3.4.0
was tagged immediately on top of that merge; the next tag picking this
section up is v3.5.0).

Combines PR #9's schema work with the host/vm scripts reorg
already on main. Next tag is up to the maintainer (main was tagged
v3.3.0 from the post-PR-7 state; PR #9's merge brought the schema
additions, tagged v3.4.0).

### Added

- **`scripts/host/verify-{ls,cat,info,stat,tree,parts}.sh`** —
  generic host-side ops, parameterized via `--binary <path>`. Each
  invokes the consumer binary's matching subcommand (`<bin> ls
  <image> <path>`, etc.) and asserts against expected output.
  Output-format conventions documented in each script's header
  (one entry per line for ls; raw bytes for cat; free-form text
  with a `key: value` convention for stat/info; sha256-of-output
  for tree). Pulled out of consumer projects (ext4-win-driver,
  erofs-win-driver) where they were duplicated as fs-locked copies.
- **`run-tests.sh` ship phase** — when the matched scenario set has
  any `host: vm` recipe step, the script now ships harness
  `scripts/vm/` + the consumer's `scripts/vm/` (if present) + the
  consumer's binary to the VM via tar+ssh / scp. Idempotent;
  always runs unless `--no-ship` is passed. Closes the gap where
  fresh consumers / fresh VMs hit "scripts not found" on the first
  vm-side scenario.
- **`run-tests.sh --no-ship`** flag — opts out of the ship phase
  for faster iteration when the VM is pre-staged manually.
- **`harness.toml [run].vm_build_command`** (optional) — if set,
  `run-tests.sh` ships the full consumer source tree (excluding
  `target/`, `.git/`, etc.) and runs `<command>` over SSH from
  `<vm.workdir>` after ship. Use case: consumers who prefer to
  build on the VM rather than cross-compile from host.
- **`scenario.volume_params`** — typed property on the scenario
  schema (`additionalProperties: true` so each consumer can
  document its own inner field set). Used by consumers whose
  recipes build the image at scenario-time and substitute
  `{scenario.volume_params.<field>}` into op templates (e.g.
  `mac-format` with size_mib/label/alloc_unit_size).

### Changed

- **`scripts/win/` → `scripts/vm/`** (BREAKING for any consumer
  that hand-references the path). Rationale: `scripts/host/` and
  `scripts/vm/` are the two `executed-during-a-test-run`
  directories; `host`/`vm` mirrors the recipe-step `host:` field.
  `win/` was never quite right because the ops are technically
  POSIX-shell-callable wherever the runner can SSH; vm/ describes
  what they're FOR rather than where they came from.

### Removed

- `scenario.mount` and `scenario.mount_args` from the schema —
  these were stale after the v3.0.0 runtime removal of
  `Scenario.mount` / `Scenario.mount_args` / `MountSpec`. v3.0.0
  missed the matching schema cleanup; landed here.

### Migration

- After bumping `vendor/fs-test-harness` to this release: open
  your `harness.toml` and replace any
  `{vm.harness_root}/scripts/win/` template path with
  `{vm.harness_root}/scripts/vm/`. Mechanical rename.
- Optional: drop your consumer-local `scripts/verify-*.sh` files
  if they were copies of the harness-shipped ones; reference the
  generic ones via `{harness_root}/scripts/host/verify-*.sh
  --binary {binary} {scenario.image} {step.path} ...` from your
  `[ops.verify-*]` op-defs.

### Notes

The deliberate posture on the schema: the scenario schema stays
`additionalProperties: false` (typo-trapping is the win). Consumer-
defined fields are added explicitly, typed, with a
`{scenario.<dotted.path>}` substitution rationale documented in
the schema. Novel consumer-only fields require a one-line schema
PR — gating prevents grab-bag accumulation.

`_doc`, `_notes`, `_attempts`, `evidence_link` were already present
since v2.0.0; recipe steps remain `additionalProperties: true` so
step-level fields don't need schema declarations.

## [3.1.0] - 2026-05-10

Single-entrypoint + v2 dispatch + vm-side ops + env-overrides. Tagged
from main after PR #6 merged; backfilled here from the prior
`[Unreleased]` placeholder (the placeholder was authored before the
tag and never renamed).

### Added

- `scripts/run-tests.sh` — single-entrypoint replacement for the
  old `setup-local.sh` + `test-windows-matrix.sh` two-step. First-
  run prompts inline; subsequent runs go straight to run. `--help`
  is built from the leading comment block.
- v2-mode dispatch in `run-tests.sh`: detects matched scenarios
  with non-empty `recipe`, sources `.test-env` (exports VM_HOST /
  VM_WORKDIR / VM_IMAGE_DIR / SSH_KEY for the runner), runs
  `cargo run --bin run-matrix` LOCALLY on the orchestrator. Per-
  step ssh + scp tunnel to the VM as needed.
- `scripts/win/_lib.ps1` + 7 generic vm-side ops (`win-write` /
  `win-mkdir` / `win-rmdir` / `win-unlink` / `win-rename` /
  `win-cat-via-mount` / `win-ls-via-mount`). Each is self-contained
  mount-do-unmount within one SSH session, parameterised by
  `-BinaryCmd` + `-ReadyLine`. (Renamed to `scripts/vm/` in the
  next release.)
- Substitution flat tokens: `{image_dir}`, `{vm.workdir}`,
  `{vm.harness_root}`. `HARNESS_DIR` / `HARNESS_IMAGE_DIR` env
  overrides for those.
- `VM_HOST` / `VM_WORKDIR` / `SSH_KEY` env overrides in
  `dispatch::run_vm` + `dispatch::run_builtin_ship`. Empty-string
  config values treated as unset (lets `harness.toml` ship
  placeholder defaults that `.test-env` supplies actuals for).
- `harness_self_version` helper in `_lib_harness.sh`; printed at
  the top of every run as `[harness] fs-test-harness
  <git-describe>` so consumers see which checkout is in play.

### Removed

- `scripts/setup-local.sh` — folded into `run-tests.sh` first-run flow.
- `scripts/test-windows-matrix.sh` — replaced by `run-tests.sh`.

## [3.0.0] - 2026-05-10

**Breaking change**: removed the legacy whole-scenario PowerShell
dispatch path. The runner now drives every scenario through the
recipe-step dispatcher introduced in `2.0.0`.

### Removed

- `Scenario.ops` (and the `operations` serde alias) — recipes now
  use `recipe: Vec<Step>` exclusively.
- `OpSpec` type alias — superseded by `Step` (free-form JSON value).
- `TomlAdapter`, the `Adapter` trait, and `OpResult` — the old
  whole-scenario adapter shape used to spawn `run-scenario.ps1`. No
  longer needed; consumers writing custom Rust drivers can call
  `dispatch::run_recipe` directly.
- `scripts/run-scenario.ps1` — legacy per-scenario PowerShell driver.
- `MountSpec` (per-scenario) and `MountSection` (in `harness.toml`)
  — the auto-mount lifecycle is gone; recipes that need a mount
  declare it as an explicit `op` step.
- `Scenario.mount_args` (`["--rw"]`-style argv) — same reason.
- Schema: dropped `ops` / `operations` properties and `$defs/op`
  from `test-matrix.schema.json`; dropped the `mount` section from
  `harness.schema.json`.

### Migration

- Replace per-scenario `ops: [...]` with `recipe: [...]`. Each step
  needs at least an `op` field; `host: "host"` or `host: "vm"` if
  the op-def doesn't already pin one.
- Remove `[mount]` from `harness.toml`. Express the mount as a
  recipe step using an `[ops.mount]` table form (with explicit
  `host = "vm"` and a `command` template).

### Kept

- The bare-string `[ops]` shorthand (`ls = "{binary} ls {image} {path}"`)
  — pure config sugar for `{ host: "vm", command: <string>,
  expect_exit: 0 }`.
- The `type` field as an alternative spelling of `op` on a recipe
  step — informal alias preserved.

## [2.0.0] - 2026-05-10

Recipe-shaped scenarios + per-step dispatcher. See the v2 design
notes in `docs/`. Tagged today as the first stable release of the
recipe model.

## [0.1.0] - 2026-05-07

Initial extraction from `rust-fs-ntfs` and `ext4-win-driver`. MIT
licensed.

### Added

- `scripts/` Mac-side orchestration: `claim-scenario.sh`,
  `update-scenario-status.sh`, `reset-non-passed.sh`, `setup-local.sh`,
  `test-windows-matrix.sh`.
- `scripts/setup-windows-vm.ps1` one-time VM provisioning; consumers
  pass their own winget package list via `harness.toml [vm.packages]`.
- `scripts/run-scenario.ps1` Windows-side per-scenario executor;
  parameterised over the consumer's `[ops]` table.
- `runner/` Rust crate. `lib.rs` exposes the public API
  (`Harness`, `Scenario`, `Adapter`, `TomlAdapter`); `bin/run-matrix.rs`
  is the libtest-mimic entry point.
- `schemas/test-matrix.schema.json`, `schemas/harness.schema.json`.
- `docs/consumer-integration.md`, `docs/architecture.md`,
  `docs/triage-protocol.md`, `docs/multi-agent-protocol.md`.
- `examples/minimal/` smallest viable consumer.

### Known limitations

- `run-matrix` only does useful work on Windows (it shells out to the
  consumer's binary and the WinFsp/format.com toolchain). Trials are
  marked ignored on macOS / Linux but compile-check.
- Post-verify hooks (`fsck`, `chkdsk`) are configured via
  `[tools]` in `harness.toml`; we do not yet stream the post-RW image
  back to the Mac for off-VM verification, that is the consumer's
  responsibility.
- `setup-windows-vm.ps1` ships only the cross-consumer essentials
  (rustup, gnullvm toolchain, optional winget packages). Consumers
  that need something more exotic (LLVM-MinGW, qemu-img, WinFsp,
  libclang) declare them in `[vm.packages]`.
