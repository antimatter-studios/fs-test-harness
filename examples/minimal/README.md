# minimal example

Smallest plausible `fs-test-harness` consumer. Two files:

- `harness.toml` — adapter config.
- `test-matrix.json` — one scenario.

This example references a fictional `myfs.exe` driver that does not
exist, so any actual run will *fail* — and that is the point: the
diagnostic tree the harness emits on a forced failure is the easiest
thing to inspect first.

## Walk through it

From the harness repo root:

```sh
# 1. Build the runner.
cd runner && cargo build --release && cd ..

# 2. Point run-matrix at this example. We deliberately skip the SSH
#    + tar dance; the runner can be invoked directly to demonstrate
#    the local control flow.
./runner/target/release/run-matrix \
    --config examples/minimal/harness.toml \
    --filter minimal-list-root
```

You should see:

- A `test-diagnostics/run-<UTC>/minimal-list-root/` directory appear.
- `manifest.json` with `verdict: "errored"` because `myfs.exe` is not
  present.
- `op-trace.jsonl` showing the single `ls` op the runner attempted to
  execute, with the substituted command line and the spawn error.

That tree is the same shape a real consumer's failed scenario produces —
the only difference is that real consumers also have `mount-stdout.txt`,
`mount-stderr.txt`, and per-op `opNN-stdout.txt` files populated.

## Next step

Read [`../../docs/consumer-integration.md`](../../docs/consumer-integration.md)
for the real onboarding flow (SSH setup, `run-tests.sh` first-run
bootstrap, claim/run/update loop).
