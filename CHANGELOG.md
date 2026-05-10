# Changelog

All notable changes to fs-test-harness will land here. The format
loosely follows Keep a Changelog; semver applies from `2.0.0` onward.

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
