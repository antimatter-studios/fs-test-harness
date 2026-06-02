# Changelog

All notable changes to fs-test-harness will land here. The format
loosely follows Keep a Changelog; semver applies from `2.0.0` onward.

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
