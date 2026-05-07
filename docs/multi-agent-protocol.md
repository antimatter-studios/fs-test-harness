# Multi-agent protocol

Rules for running multiple agents (humans, LLMs, CI workers) against
the same `test-matrix.json` concurrently. The harness is designed for
this; the rules below keep them from stepping on each other.

## Sessions

Each agent picks a **session ID** for the duration of its run, e.g.
`agent-3f7c` or `ci-build-481`. The session ID is a free-form string
that ends up in the scenario's `claimed-<session>`, `passed-<session>`,
`failed-<session>` status. Pick something traceable.

```sh
export FS_HARNESS_SESSION="agent-3f7c"
```

The claim/update/reset scripts read `FS_HARNESS_SESSION` (or accept
`--session`).

## Claim before you run

```sh
bash harness/scripts/claim-scenario.sh
```

What it does:

1. Reads `test-matrix.json`.
2. Picks the first `pending` scenario.
3. Atomically rewrites the matrix with that scenario's status set to
   `claimed-<session>` (via `mktemp` + `mv`).
4. Reads the file back to verify *this* session won the race.
5. On collision, retries with the next pending scenario, up to 16
   attempts with jittered backoff.
6. On success, prints the scenario name to stdout. On no-pending,
   exits 1.

```sh
SCENARIO=$(bash harness/scripts/claim-scenario.sh)
bash harness/scripts/test-windows-matrix.sh "$SCENARIO"
```

## Run, then update

The runner does *not* mutate scenario status — it only writes
`results.json` and per-scenario diag artefacts. Status transitions
are the agent's responsibility, via:

```sh
bash harness/scripts/update-scenario-status.sh "$SCENARIO" passed-"$FS_HARNESS_SESSION" \
    --evidence-link "test-diagnostics/run-<UTC>/$SCENARIO"
```

Permitted transitions:

- `claimed-<session>` → `passed-<session>` | `failed-<session>` | `errored-<session>`
- any → `blocked-needs-<feature>` (when the agent decides the scenario
  can't run yet — e.g. requires a CLI subcommand that doesn't exist)
- any → `pending` (via `reset-non-passed.sh`, see below)

The script refuses to overwrite a status that doesn't have your session
in it, except via `--force`. This catches sessions writing past each
other.

## Reset between passes

```sh
bash harness/scripts/reset-non-passed.sh
```

Atomically rewrites `test-matrix.json`: every scenario whose status is
not `passed-*` is set back to `pending`. Idempotent. Use between
matrix runs when you want to retry the failed/blocked subset.

## Concurrency boundaries

What's safe in parallel:

- **Multiple agents claiming scenarios** from the same matrix. Atomic
  rename + read-back-verify is the load-bearing primitive.
- **Multiple agents on different VMs.** Use distinct
  `harness.toml [vm.workdir]` paths so source-tar extracts don't trample.

What's NOT safe in parallel:

- **Two agents pointing at the same VM.** WinFsp drive-letter
  assignment, mount registration, and the fixed `MOUNT_LOCK` mutex in
  the runner all assume a single concurrent mount per VM. If you must
  share a VM, serialise externally — the harness will not save you.
- **Editing `test-matrix.json` by hand while a session is claimed.**
  You will race the atomic-rename pattern. Either pause the agents or
  edit a scenario while it's in `pending` state.

## Crashed sessions

If an agent dies mid-scenario, its claim is left in the matrix as
`claimed-<session>` indefinitely. To recover:

1. Confirm the session is dead (no live process owns it).
2. `bash harness/scripts/reset-non-passed.sh` will sweep it back to
   `pending`. Or hand-edit one row's status.

There is no liveness check; we lean on the simpler "agents tidy up
their own mess" rule. Agents that need automatic timeout can wrap
their loop with their own watchdog.

## Recommended agent loop

```sh
export FS_HARNESS_SESSION="agent-$(uuidgen | head -c 8)"
while SCENARIO=$(bash harness/scripts/claim-scenario.sh); do
    if bash harness/scripts/test-windows-matrix.sh "$SCENARIO"; then
        bash harness/scripts/update-scenario-status.sh "$SCENARIO" \
             "passed-$FS_HARNESS_SESSION"
    else
        bash harness/scripts/update-scenario-status.sh "$SCENARIO" \
             "failed-$FS_HARNESS_SESSION"
    fi
done
echo "no more pending scenarios"
```

Add backoff between iterations if you're sharing a VM and want to be
polite to other sessions.
