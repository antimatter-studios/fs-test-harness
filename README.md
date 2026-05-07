# fs-test-harness

A reusable Mac-orchestrator + Windows-VM-agent harness for exercising
filesystem driver / formatter projects against real test images.

Originally extracted from two near-identical copies (`rust-fs-ntfs` and
`ext4-win-driver`). Both copies followed the same shape:

1. Build a disk image with a reference tool (`format.com`, `mkfs.ext4`, ...).
2. Exercise our driver against the image (mount, read, write, ...).
3. Verify with a structural checker (`chkdsk`, `fsck.ext4`, ...) and
   round-trip read-back.

This repo factors that shape out as a generic harness whose
filesystem-specific bits are configured per-consumer via `harness.toml`
plus a `test-matrix.json`.

## Status

Version `0.1.0`. Not yet published anywhere; consumers vendor it as a
git submodule or path-dep until we tag a release. See
[`CHANGELOG.md`](./CHANGELOG.md).

## What is in here

```
fs-test-harness/
  scripts/                   # Mac-side orchestration + VM agent template
    setup-local.sh           #   one-time prompt-driven setup
    setup-windows-vm.ps1     #   one-time VM provisioning (winget)
    test-windows-matrix.sh   #   tar source -> ssh -> remote cargo test -> pull diag
    claim-scenario.sh        #   atomic claim from test-matrix.json
    update-scenario-status.sh#   atomic status update (used by agents)
    reset-non-passed.sh      #   reset non-passed scenarios to pending
    run-scenario.ps1         #   per-scenario Windows executor (template)
  runner/                    # Rust crate (lib + `run-matrix` bin)
  schemas/                   # JSON Schemas (matrix + harness.toml)
  docs/
    consumer-integration.md  # PRIMARY DOC: how a new project plugs in
    architecture.md          # topology, lifecycle, state machine
    triage-protocol.md       # interpreting a failed scenario
    multi-agent-protocol.md  # concurrency rules for parallel agents
  examples/minimal/          # smallest plausible consumer
```

## Quick links

- New consumer onboarding: [`docs/consumer-integration.md`](./docs/consumer-integration.md)
- How the pieces fit together: [`docs/architecture.md`](./docs/architecture.md)
- Diagnosing a red scenario: [`docs/triage-protocol.md`](./docs/triage-protocol.md)
- Two-or-more agents on one work list: [`docs/multi-agent-protocol.md`](./docs/multi-agent-protocol.md)

## Adapter shape, in one paragraph

Consumers ship a `harness.toml` declaring their binary, VM connection,
and a per-op command-template table (`{binary} ls {image} {path}`). The
harness substitutes scenario fields into those templates and runs them
on the VM. For ops that need real Rust (FFI probes, in-process mount
tests), consumers can instead `impl Adapter` and link the runner as a
library; this is the escape hatch, not the default. See
[`docs/consumer-integration.md`](./docs/consumer-integration.md) for the
full contract.

## License

`MIT`. See [`LICENSE`](./LICENSE). The harness is pure orchestration
(bash, PowerShell, libtest-mimic, our own Rust); no copyleft input.

Consumer projects that themselves link copyleft components (e.g. the
WinFsp Rust bindings under GPL-3.0) are unaffected — the harness
neither imports nor distributes those components. Consumers re-license
their *own* binary output as their license requires; the harness stays
permissively licensed.
