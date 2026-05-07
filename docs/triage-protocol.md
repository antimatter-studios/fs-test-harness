# Triage protocol

How to interpret a failed scenario. Order matters: each step narrows
the search space cheaply before the next, more expensive one.

## 1. Top-level verdict

Open the run's `results.json`:

```json
{
  "name": "ro-list-root",
  "verdict": "failed",
  "duration_ms": 1842,
  "diag_dir": "ro-list-root"
}
```

Verdicts:

- `passed` — all ops met expectations.
- `failed` — at least one op produced output that did not match its
  `expect_*` fields. The driver ran but disagreed with reality.
- `errored` — an op (or the mount, or post-verify) errored before
  producing comparable output. Could be a missing binary, a spawn
  failure, a timeout, an unmappable template token, etc.
- `blocked` — the scenario's status was `blocked-needs-*`; the runner
  skipped it deliberately. Not a bug; agents bookkeep these.

## 2. Per-scenario manifest

`<diag-dir>/manifest.json` carries the verdict plus a structured op
trace:

```json
{
  "scenario": "ro-list-root",
  "verdict": "failed",
  "started_at": "2026-05-07T19:23:04Z",
  "duration_ms": 1842,
  "mount": { "ok": true, "stdout_path": "mount-stdout.txt" },
  "ops": [
    {
      "index": 0, "type": "ls", "ok": false,
      "reason": "expect_names mismatch",
      "expected": [".", "..", "lost+found"],
      "actual":   [".", "..", "lost+found", "stale-marker.txt"]
    }
  ],
  "post_verify": null
}
```

If the verdict is `errored` rather than `failed`, the manifest has an
`error` field at top level pointing at the stage that died (mount,
op-N, post-verify). Skip to step 4.

## 3. Per-op evidence

For a failed op, look in this order:

1. **`op-trace.jsonl`** — one JSON line per op. Has the substituted
   command line the runner actually executed, the captured stdout/
   stderr lengths, and the per-op verdict.
2. **`opNN-stdout.txt`** + **`opNN-stderr.txt`** — full output of the
   substituted command. (`NN` is the op index; `op00`, `op01`, …)
3. **`scenario.json`** — the scenario as the PS runner saw it. Compare
   to `test-matrix.json` to be sure no template substitution went wrong.

## 4. Mount + post-verify evidence

For `errored` mounts:

- **`mount-stdout.txt`** — contains the `ready_line` regex match (or
  doesn't, which is the failure mode).
- **`mount-stderr.txt`** — driver panics, missing DLLs, missing WinFsp,
  unmappable image paths.
- **`ps-stdout.txt`** + **`ps-stderr.txt`** — full PowerShell transcript
  including the runner's own debug output, drive-letter selection
  logic, and any errors before/after the mount lifecycle.

For post-verify failures:

- **`post-verify-stdout.txt`** + **`post-verify-stderr.txt`** — the
  reference checker's output (`fsck`, `chkdsk`, etc.). The exit code
  is checked against `[post_verify] expect_exit`.

## 5. Reproducing locally

Once you have a hypothesis, drop the test harness and reproduce by
hand:

1. Copy the image referenced in `scenario.json` to a known dir.
2. Run the substituted commands from `op-trace.jsonl` directly.
3. Compare to your hypothesis.

This loop is *much* faster than re-running the full matrix harness for
hypothesis testing. Use the harness for regression confirmation; use
direct invocation for diagnosis.

## 6. Patching + recording the fix

After you fix the driver, update the scenario:

- Add an entry to `_attempts` in `test-matrix.json` describing the
  hypothesis, the change, and the result.
- Reset the scenario's status with
  `bash harness/scripts/update-scenario-status.sh <name> pending`.
- Re-run the harness to confirm.

If the scenario passes, the next run sets it to `passed-<session>` and
writes `evidence_link` pointing at the green diag dir.

## Anti-patterns

- **Don't hand-edit a scenario's `expect_*` to make it pass.** That
  silently widens the contract. Either the driver is wrong (fix it) or
  the expectation was wrong (write down *why* it changed in `_notes`).
- **Don't ignore `errored` to find more `failed`.** An errored mount
  often poisons every downstream scenario via stuck drive letters or
  WinFsp host state.
- **Don't trust a green run after editing only `expect_*`.** Re-run
  with a clean diag dir.
