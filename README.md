# fs-test-harness

> Reusable Mac-orchestrator + Windows-VM-agent test harness for filesystem driver projects.

[![CI](https://github.com/antimatter-studios/fs-test-harness/actions/workflows/ci.yml/badge.svg)](https://github.com/antimatter-studios/fs-test-harness/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Rust 1.79+](https://img.shields.io/badge/rust-1.79%2B-orange.svg)](https://www.rust-lang.org)
[![Status: alpha](https://img.shields.io/badge/status-alpha%20(0.1.0)-yellow.svg)](./CHANGELOG.md)

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
| `scripts/` | Mac-side bash + Windows-side PowerShell. `claim-scenario.sh`, `update-scenario-status.sh`, `reset-non-passed.sh`, `setup-local.sh`, `test-windows-matrix.sh`, `run-scenario.ps1`. |
| `schemas/` | JSON Schema for `harness.toml` and `test-matrix.json`. |
| `docs/` | Long-form: consumer integration, architecture, triage, multi-agent protocol. |
| `examples/minimal/` | Smallest viable consumer config. |
| `tests/` | Self-test fixtures: state-machine bash test + a `mock-fs` Cargo crate that stands in for a real driver in CI. |
| `.github/workflows/` | CI: lint, runner unit tests, state-machine integration, end-to-end mock-scenario on `windows-latest`. |

## Quickstart

For a new consumer:

```sh
# 1. Vendor the harness as a submodule (or path-dep) at <consumer>/harness/.
git submodule add https://github.com/antimatter-studios/fs-test-harness.git harness

# 2. Drop a harness.toml + test-matrix.json next to your Cargo.toml.
cp harness/examples/minimal/harness.toml ./harness.toml
cp harness/examples/minimal/test-matrix.json ./test-matrix.json
$EDITOR harness.toml   # point [project.binary] at your driver, fill [vm.*]

# 3. One-time setup: prompts for VM host / key, writes .test-env.
bash harness/scripts/setup-local.sh

# 4. Run the matrix.
bash harness/scripts/test-windows-matrix.sh
```

Full contract: [`docs/consumer-integration.md`](./docs/consumer-integration.md).
Architecture overview: [`docs/architecture.md`](./docs/architecture.md).
Diagnosing a red scenario: [`docs/triage-protocol.md`](./docs/triage-protocol.md).
Concurrent-agent rules: [`docs/multi-agent-protocol.md`](./docs/multi-agent-protocol.md).

## Self-test

The CI pipeline reproduces locally — useful before pushing a change to
the harness machinery itself. Each command is independent.

```sh
# Shell scripts: syntax + lint.
bash -n scripts/*.sh tests/*.sh
shellcheck -x scripts/*.sh

# Runner: format, lint, unit tests.
cargo fmt --manifest-path runner/Cargo.toml --all -- --check
cargo clippy --manifest-path runner/Cargo.toml --all-targets -- -D warnings
cargo test  --manifest-path runner/Cargo.toml --all-features --no-fail-fast

# State machine: claim/update/reset against a temp-dir matrix.
bash tests/state-machine.sh

# End-to-end loop with a stand-in driver (Linux/macOS will compile it;
# the actual mount path runs on windows-latest in CI).
cargo build --manifest-path tests/mock-fs/Cargo.toml --release
```

The end-to-end **mock-scenario** job in CI builds `mock-fs` (a tiny
no-op CLI), points the harness at it via a fixture `harness.toml`, and
asserts that two scenarios go from "claim" through mount-with-ready-line
through ops through teardown to a `verdict: passed` manifest — without
any real filesystem driver, WinFsp, or SSH'd VM.

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

Version is `0.1.0` — extraction is complete and the harness backs
`ext4-win-driver`'s migration in progress; expect minor breaking
changes until `1.0`.
