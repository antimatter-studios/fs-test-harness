# Changelog

All notable changes to fs-test-harness will land here. The format
loosely follows Keep a Changelog; semver applies once we tag `1.0.0`.

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
