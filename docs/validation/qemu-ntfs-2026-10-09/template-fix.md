# Recipe reference validation — 2026-10-09

Implementation: `e1520590d32aa2fa5fd0b0c91d915cbc7ffd44d5` on `feat/qemu-local-vm`, addressing
harness issue #45. No NTFS driver code, matrix, expectations, or budgets
were changed. The original 71-pass / 1-fail NTFS result remains unchanged.

## Behavior and regressions

The label regression was red: `-Label '{step.label}'` emitted the literal
`{scenario.volume_params.label}`. It now emits the scenario's actual label.
Recipe dispatch rejects missing required references before executing commands
or transfers. Optional references still work. Cycles, depth beyond 32, and
templates exceeding 1 MiB fail explicitly; Unicode and literal braces survive.

An independent static audit identified four uses of the consumer's literal
`{N}` batch marker. A second regression caught its accidental interpretation
as a harness token. Recipe fields now expand reserved harness references
while preserving consumer markers. Scenario, flat and structured values remain
literal data. See [the vocabulary](../../vocabulary.md) for escaping syntax.

## Validation

- All `chore check` gates passed. All 42 baseline Rust tests remain passing;
  the suite now has 54 tests. All 29 provider tests remain passing.
- Rust line coverage rose from 79.26% to 81.53%.
- The actual checked Rust engine audited all 72 scenarios / 438 steps at
  NTFS `fe450f1798993cf1c8d7b4866a0855761d9aa5c6`, with zero expansion errors.
  The audit used synthetic transport paths and executed no driver commands.
  Input hashes match the original validation; [results](template-fix-results.json)
  record the method, hashes and measurements.
- Complete real Windows smoke passed first time: all 18 positive steps
  executed, then all three canary steps produced the expected content-mismatch
  failure. No step was skipped and no CLIXML leaked.
- The Windows read command resolved its nested recipe path to `/hello.txt`.
  Independent Windows write and unlink output retained `X:\scratch-{N}.txt`.
  The [transcript](smoke-template-fix.txt) retains the verdicts.

The raw smoke transcript has 142 lines / 8563 bytes;
its SHA-256 is `422e1b17ef642d1fd65d5217c4479154a8b76c981176ddbd8c45df4833675f7c`. The published copy replaces
local paths and normalizes line endings; the original logs and step diagnostics
are retained privately alongside the VM state.

This checks the harness fix and template compatibility. It does not constitute
a new full NTFS matrix run or establish VMware/macOS parity. The consumer's
stale rejection expectation and output budget remain separate test/config work.
