# Architecture

A three-layer system: **Mac orchestrator + Windows VM agent + shared
state file**.

## Topology

```
+-- Mac (developer laptop) ---------------------+
| <harness>/scripts/run-tests.sh                |
|   tar consumer source -> ssh ->               |
|   remote `cargo run --bin run-matrix` ->      |
|   pull test-diagnostics tree                  |
+----------------------+------------------------+
                       | ssh
                       v
+-- Windows VM ---------+-----------------------+
| run-matrix bin (libtest-mimic, one trial /    |
|   scenario):                                  |
|   for each scenario:                          |
|     adapter.build_scenario_json -> scenario.json
|     spawn run-scenario.ps1                    |
|     parse VERDICT= marker                     |
+----------------------+------------------------+
                       | spawn
                       v
+-- per-scenario PowerShell --------------------+
| <harness>/scripts/run-scenario.ps1            |
|   A. resolve image                            |
|   B. spawn `<binary> mount <image> ...`,      |
|      wait for ready_line regex match          |
|   C. iterate ops[]: template substitution OR  |
|      built-in PS file op (write/mkdir/...)    |
|   D. stop mount process                       |
|   E. (optional) post_verify command           |
|   F. write manifest.json + op-trace.jsonl     |
+-----------------------------------------------+
```

The mount lifecycle is process-global on Windows (drive-letter
assignment, WinFsp host singletons), so scenarios serialise via a
`Mutex` in the runner *and* `--test-threads=1` is forced. Concurrent
runs across separate VMs are independent.

## Lifecycle: state machine for one scenario

```
   pending
     |
     | claim-scenario.sh "<session>"
     v
   claimed-<session>
     |
     | run-matrix executes the scenario
     |
     +-- VERDICT=passed --> update-scenario-status.sh ... passed-<session>
     +-- VERDICT=failed --> update-scenario-status.sh ... failed-<session>
     +-- VERDICT=errored -> update-scenario-status.sh ... errored-<session>
     +-- (agent decision) -> blocked-needs-<feature>
```

The runner does not write status itself -- agents drive transitions
via the `claim-scenario.sh` / `update-scenario-status.sh` pair. The
runner only writes `result.json` (per scenario) and `results.json`
(aggregate) under the diag tree; status is the human / agent loop's
job to maintain.

## State storage

`test-matrix.json` at the consumer repo root is the single source of
truth. Schema is documented in [`../schemas/test-matrix.schema.json`](../schemas/test-matrix.schema.json).

All mutations are atomic via `mktemp` + `mv`. The `claim-scenario.sh`
script also reads back after the rename and verifies the session won
the race; if not, it retries with the next pending scenario, up to 16
attempts.

## Diagnostics layout

Pulled to `<consumer-root>/test-diagnostics/run-<UTC>/` after each Mac
invocation:

```
test-diagnostics/run-<UTC>/
  run-manifest.json         project, host, sha, scenario count
  results.json              aggregated pass/fail (one element per scenario)
  <scenario-name>/
    manifest.json           per-scenario verdict, op trace, timing
    scenario.json           the resolved scenario JSON the PS runner saw
    mount-stdout.txt        mount-cmd stdout (when [mount] declared)
    mount-stderr.txt        mount-cmd stderr
    op-trace.jsonl          one line per op: input, output, verdict
    opNN-stdout.txt         per-op stdout (template ops only)
    opNN-stderr.txt
    post-verify-stdout.txt  (when [post_verify] runs)
    post-verify-stderr.txt
    ps-stdout.txt           full PowerShell stdout (for verdict markers)
    ps-stderr.txt
    result.json             machine-readable verdict
```

The persistent contract is `manifest.json` + `op-trace.jsonl`. Together
they let an agent walking into a diag dir cold reconstruct what was
tested and which op failed without re-running anything.

## Concurrency

- **Multiple agents** can claim from the same matrix.json in parallel.
  The atomic-rename + read-back-verify pattern prevents two agents
  taking the same scenario.
- **Per VM**: scenarios serialise via `MOUNT_LOCK` + `--test-threads=1`.
- **Per agent session**: each agent SHOULD point `VM_WORKDIR` at a
  session-namespaced directory so two concurrent extractions don't
  trample each other on the same VM. The `run-tests.sh` first-run
  bootstrap prompts for this.

## Re-entrancy

- `reset-non-passed.sh` idempotently resets every non-`passed-*`
  scenario back to `pending`. Use between passes.
- `run-scenario.ps1` cleans stale mount processes via `Stop-Process
  -Force` at the start of each scenario; no VM power cycle is needed
  in the normal failure modes.

## Adapter model

Two ways to plug behaviour in (the entire reason this harness was
extracted):

- **Option A (default) -- TomlAdapter.** `harness.toml [ops]` declares
  shell command templates per op type. The runner substitutes
  per-scenario tokens and runs them on the VM via `cmd /c`. This is
  enough for any project whose driver is a CLI binary; both NTFS and
  ext4 ports fit cleanly here.

- **Option B (escape hatch) -- Adapter trait.** Consumers can link the
  runner crate as a library and `impl Adapter for MyDriver`. For ops
  that need real Rust (FFI probe, in-process mount test) the trait
  method runs in-VM Rust instead of shelling out. Less common;
  documented as the escape hatch, not the default.
