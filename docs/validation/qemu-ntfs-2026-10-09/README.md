# Raspberry Pi QEMU validation — 2026-10-09

**Full parity is not accepted.** Fresh provisioning and the WinFsp smoke test
passed. The unchanged NTFS matrix reported **71 passed, 1 failed**, with three
recipe steps unexecuted after that failure. Four requested online scans used
offline fallbacks, and the output exceeded the consumer's budget. macOS work
remains deferred.

## Inputs and environment

- Harness: `feat/qemu-local-vm`, implementation commit
  `8e4a5ee974cd2557ec35e97d661732ccdf64e01c`.
- Consumer: `rust-fs-ntfs` at
  `fe450f1798993cf1c8d7b4866a0855761d9aa5c6` (0.9.0).
- Raspberry Pi 5, Linux ARM64, QEMU 10.0.13 with KVM; guest has two vCPUs,
  4 GiB RAM and a sparse 128 GiB disk.
- Fresh Windows 11 IoT Enterprise LTSC Evaluation ARM64 installation from the
  pinned media manifest, prepared and provisioned through this branch.
- WinFsp 2.1.25156 and OpenSSH 10.0.0.0p2-Preview; consumer helper
  `rust-img-vhd` 0.6.0 built for `aarch64-pc-windows-gnullvm` with LLVM MinGW
  20261006 and Rust 1.98.1. The helper and `libunwind.dll` were installed in
  the guest's `C:\fswth-tools` directory.
- NTFS's source, matrix, recipes, configuration and concurrency were unchanged.
  The consumer's existing `max_parallel = 4` and 900-second step timeout were
  retained. Only its ignored `.test-env` selected this VM and work directory.

[results.json](results.json) records input hashes, exact commits, guest versions,
each scenario's outcome, executed-step counts and the collected Windows
verdicts. The consumer's tracked worktree was clean before and after the run;
its matrix and configuration hashes also matched the initial values.

## Results

| Check | Observed result |
|---|---|
| `chore check` | Passed, including all 42 Rust tests and 29 new VM tests |
| Coverage | Rust line coverage unchanged at 79.26%; VM Python coverage 87% |
| Real Windows policy regression | Passed |
| Fresh provisioning | Passed after the two harness fixes described below |
| Repeat `provision` | Passed in 20.712 seconds |
| Repeat `up` | Reused the running VM in 0.215 seconds |
| WinFsp smoke | Passed in 508.181 seconds: 18 positive steps; all three canary steps ran and the wrong-content assertion failed as intended |
| Full NTFS matrix | Exit 101: 71 passed, 1 failed, 0 ignored, no automatic retries |
| Recipe completeness | 435 of 438 steps executed; the failed case stopped before its final three operations |
| Matrix timing | 3,737.342 seconds including setup/build/shipping/cleanup; runner itself 3,521.50 seconds |
| VM reuse | Same Windows boot time before smoke and after the complete NTFS run: `2026-10-09T14:06:50.5627790Z` |

The 16 GiB and both 4 GiB scenarios passed. The 16 GiB upload took 9 minutes
56 seconds and stayed inside the existing timeout. The VM was not rebooted
between provisioning, the two smoke matrices and the NTFS matrix.

The retained [NTFS transcript](ntfs-output.txt) and
[smoke transcript](smoke-output.txt) include their actual verdicts. Local
checkout/state paths were replaced with `${CONSUMER}`, `${HARNESS}` and
`${VM_STATE}`/`${CARGO_TARGET_DIR}`; line endings and trailing whitespace were normalized for
review. The original logs remain intact in the retained evidence. The raw
NTFS log's hash and byte count are in `results.json`.

## Remaining gates

### Expected rejection now succeeds

`win-format-win-write-many-mac-reject-index-allocation-win-chkdsk` failed at
zero-based step 5, `mac-touch-index-allocation-rejected`:

```text
step 5 (mac-touch-index-allocation-rejected) exit Some(0) != expected 1
created file rec=298 //mac-rejected.txt
```

The recipe creates 256 files in Windows and expects the host driver to refuse
another insertion into the resulting `$INDEX_ALLOCATION` directory. The pinned
consumer's [feature documentation](https://github.com/antimatter-studios/rust-fs-ntfs/blob/fe450f1798993cf1c8d7b4866a0855761d9aa5c6/docs/features.md)
records insertion and splitting support since 0.8.0. Its `create_file` and
index-insertion implementation also describe that support. This points to a
stale negative expectation; it is not a reason to change or suppress the
matrix result in the harness.

A separate diagnostic copy of the retained Windows input reproduced the
successful insertion with the same unchanged binary. Windows then:

- Completed both [read-only chkdsk](supplemental-chkdsk-readonly.txt) and
  [online /scan](supplemental-chkdsk-scan.txt), exit 0, state `scanned`, clean.
- [Enumerated](supplemental-enumeration.txt) `mac-rejected.txt` and all 256
  original numbered files; explicit required-path checks also passed.

This supplemental check does not replace the failed scenario, execute its
missing steps inside the original matrix, or change the 71/72 result.
The remaining recipe operations were `ship-to-vm`, `win-chkdsk` and
`win-enumerate`. Resolving this expectation belongs to the consumer and was
outside the permitted edits for this branch.

### Four online scans did not run

These scenarios reported an unavailable volume snapshot for `/scan`:

- `cli-windows-interrupted-index-vcn-fsck-replay-win-verify-chkdsk`
- `foreign-fragmented-indx-512-win-enumerate-chkdsk`
- `mac-format-tiny-32mib`
- `mac-format-volume-32mib-cluster-512`

Their existing consumer code ran offline `/F /X` fallbacks and reported the
scenarios passed. The structured records explicitly retain `/scan` as
`not-scanned`; the [original scan reports](unavailable-online-scans.txt) are
included here. These are a completeness gate even though libtest reported
zero ignored tests. No VMware run of this exact consumer revision was
available for comparison, so this evidence does not establish whether the
snapshot limitation differs between providers.

### Output exceeds the consumer's limit

The raw matrix log contained **1,444 newline-terminated lines and 107,771
bytes**, above the unchanged **1,300-line / 90,000-byte** limits. The wrapper
preserved the scenario failure's exit 101; an otherwise passing run of this
size would fail the output gate with exit 65.

The consumer's budget comments derive the current row from an older
46-scenario measurement and a 56-scenario, 341-step matrix. This run selected
72 scenarios and 438 steps. Neither that budget nor the harness's output was
changed to make the run appear green.

## Fresh-install fixes included in this branch

1. A cold PowerShell readiness command took 23.201 seconds on the Pi, beyond
   the original 15-second probe. Commit `632be23` increased the bounded probe
   to 60 seconds and provisioning's readiness window to five minutes; its
   regression was red before the fix and green afterward.
2. Setting `CurrentUser=RemoteSigned` under `Process=Bypass` raised
   `ExecutionPolicyOverride` and aborted provisioning. Commit `8e4a5ee`
   aligns the process scope first and includes a regression run on real
   Windows. Fresh provisioning and repeat provisioning then passed.

These changes are in the harness. No consumer assertion or driver behavior
was changed.

## Reproduction shape

Follow [the QEMU setup guide](../../qemu-vm.md) using these commits and pinned
media. Prepare a disposable worktree of the consumer beside a worktree of
this harness branch, with `rust-fs-core` v0.3.7 providing the consumer's tier
wrapper. Build its two host tools without changing their source:

```sh
cargo build --release --locked --features harness --bin rust-ntfs
cargo build --release --locked --features cli --bin rust-fs-ntfs
```

The consumer's Windows-captured snapshot fixtures must be present. Install
its pinned Windows VHD helper and runtime dependencies in the guest. Set the
ignored `.test-env` to `VM_HOST=fswth-local`, an empty `SSH_KEY`, the consumer's
separate work directory and its ordinary image directory settings.

From the harness checkout, run smoke through `local-vm.py exec`. From the
consumer checkout, run the existing matrix wrapper through the same provider:

```sh
python3 "$HARNESS/scripts/local-vm.py" --state "$VM_STATE" exec -- \
  bash ../rust-fs-core/scripts/tier.sh --refuse-skips --refuse-ignored \
  matrix -- bash scripts/run-matrix.sh

# Collect native Windows evidence even when the matrix fails.
python3 "$HARNESS/scripts/local-vm.py" --state "$VM_STATE" exec -- \
  bash scripts/matrix-fetch-diag.sh
python3 "$HARNESS/scripts/local-vm.py" --state "$VM_STATE" exec -- \
  bash scripts/vm-clean-images.sh
```

The consumer's `chore matrix` task hardcodes VMware startup, so that lifecycle
dependency was replaced with this provider's already running fresh guest.
The same matrix runner, recipe scripts and budget wrapper were used. The
consumer's diagnostic collection and cleanup scripts were run afterward;
cleanup removed 10 retained image files and reported 112 GiB free on C:.

Load was checked before starting work. Smoke and the full matrix ran
sequentially, with a host-load gate before each. No VM reset, selective matrix
filter, retry, changed concurrency or changed timeout was used to obtain these
results.
