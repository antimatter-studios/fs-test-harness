# Changelog

All notable changes to fs-test-harness will land here. The format
loosely follows Keep a Changelog; semver applies from `2.0.0` onward.

## [Unreleased] — v3.5.0 candidate

### Added

- **`harness.toml [vm.packages]` table-form entries** — each entry
  is now either a bare winget ID string (existing behaviour) OR an
  object `{ id = "...", custom_args = "..." }`. The `custom_args`
  string is forwarded to `winget install --override "<args>"`,
  which the underlying MSI/EXE installer interprets. Closes the
  fresh-VM gap for consumers that link against winget packages
  whose default feature set is wrong (most notably `WinFsp.WinFsp`,
  whose default install is runtime-only — no headers, no .lib —
  so bindgen-using consumers like ext4-win-driver / erofs-win-driver
  hit `winfsp/winfsp.h not found` on first cargo build until they
  pass `ADDLOCAL=F.Core,F.Developer`).
- **`setup-windows-vm.ps1 -PackagesJson`** — new parameter that
  accepts the resolved [vm.packages] spec as a JSON-array string.
  Branches per entry: bare strings → `winget install --id <id>`;
  objects → adds `--override "<custom_args>"`. Backward-compatible:
  the legacy `-ExtraPackages` (bare-string array) still works and
  is merged with `-PackagesJson` (JSON entries win on duplicate IDs
  so a consumer can override a bare entry with one carrying
  custom_args).
- **`PackageSpec` enum** in `runner/src/config.rs` — untagged serde
  enum that round-trips both shapes; `VmSection.packages` is now
  `Vec<PackageSpec>`. Schema updated in `harness.schema.json`.

### Behaviour notes

- **Re-runs of an installed package with different `custom_args`** —
  `winget install` won't change a package's installed feature set
  on its own; if a consumer adds `custom_args` to an entry that's
  already installed bare, setup-windows-vm.ps1 logs a hint pointing
  at `msiexec /fa <product-code> <custom_args>` for self-repair
  rather than silently doing nothing or surprise-wiping the
  install. Self-repair is left to the operator because winget's
  ADDLOCAL re-run behaviour is version-dependent.

## [Released] — v3.4.0 (post-PR-9)

Combines this branch's schema work with the host/vm scripts reorg
already on main. Next tag is up to the maintainer (main was tagged
v3.3.0 from the post-PR-7 state; this merge brings PR #9's schema
additions, so v3.4.0 is the natural next semver bump).

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
