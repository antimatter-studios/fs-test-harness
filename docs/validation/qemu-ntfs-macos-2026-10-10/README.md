# macOS QEMU/HVF validation — 2026-10-10

**Native macOS provisioning and persistent reuse work, and the unchanged NTFS
matrix reproduces the Linux baseline exactly: 71 passed, 1 failed, 435 of 438
steps.** Full parity is still not accepted. The VMware comparison was not
measured, because the existing VMware guest would not power on (see below),
so direct provider parity remains unmeasured.

**Sign-off (later the same day): with the corrected consumer, a fresh guest
built by this branch passed the complete matrix, 72 of 72 and 438 of 438
steps, exit 0, over scp** -- see [Sign-off](#sign-off-with-the-corrected-consumer).
The VMware guest, once it could be started, proved unable to attach VHDs
reliably; QEMU replaces it.

## Inputs and environment

- Harness: `feat/qemu-local-vm`, started from `c113f23`. The NTFS matrix ran
  on tree `f8d3928`: `c113f23` plus this branch's macOS commits, before they
  were rebased onto `357a6e9`.
  [matrix-harness-delta.patch](matrix-harness-delta.patch) is the exact
  difference in `scripts/`, `runner/` and `tests/` between that tree and the
  pushed head (`git apply --unidiff-zero` on `f8d3928`). It holds the
  recipe-reference fix from `e152059`, the keep-awake process reaping, the
  shutdown tests' explicit SSH failure and the runner scratch-directory fix.
  Checks and smoke were rerun on the rebased code.
- Consumer: `rust-fs-ntfs` at `fe450f1798993cf1c8d7b4866a0855761d9aa5c6`,
  the same commit as the Linux report. Matrix and configuration hashes match
  the Linux report's. The tracked tree was clean before and after the run.
- Host: MacBook Pro, Apple M3 Pro, 36 GiB, macOS 26.4.1, on battery.
  QEMU 10.2.2 (Homebrew) with HVF and QEMU's emulated GICv3; Homebrew's
  `edk2-aarch64-code.fd` and `edk2-arm-vars.fd`, discovered by `prepare`.
  Python 3.14.8, macOS `bsdtar` 3.5.3 (libarchive 3.7.4).
- Guest: a fresh Windows 11 IoT Enterprise LTSC Evaluation ARM64
  installation from the pinned media manifest, prepared and provisioned
  through this branch. 2 vCPUs, 4 GiB RAM, a sparse 128 GiB disk, WinFsp
  2.1.25156, OpenSSH 10.0p2. These are the Pi's sizes.
- Consumer helper: `rust-img-vhd` 0.6.0 (with `rust-fs-core` v0.3.7, as
  the consumer's CI pins), cross-built for `aarch64-pc-windows-gnullvm` with
  Rust 1.95.0 and LLVM MinGW 20261006 (macOS universal build). It was
  installed with `libunwind.dll` in the guest's `C:\fswth-tools`, on the
  machine PATH. The Pi built the same version with Rust 1.98.1 and the Linux
  LLVM MinGW build, so the helper binaries' hashes differ.
- NTFS source, matrix, recipes, configuration, concurrency (`max_parallel =
  4`), the 900-second step timeout and the output budget were unchanged.
  Only the ignored `.test-env` selected this VM.

[results.json](results.json) records input hashes, versions, every scenario's
outcome with its step exits and Windows verdict, and every command run with
its exact text, timing, exit status, output size and output hash.

## Results

| Check | Observed result |
|---|---|
| Fresh provisioning | Passed: `prepare` 3.6 s, Windows setup 176 s after the one UEFI Shell interaction, `provision` 25 s |
| `chore check` | Passed on the pushed code (run 63): 48 Rust and 39 provider tests. Failed runs are listed below |
| WinFsp smoke | Passed: 18 positive steps; all three canary steps ran and the wrong-content assertion failed as intended; zero CLIXML |
| Repeat smoke | Passed on the same Windows boot, zero CLIXML |
| Smoke after rebase | Passed on the rebased code, 87 s, zero CLIXML |
| Shutdown and restart | Clean after the fix below; installation, activation, WinFsp and the helper preserved |
| Full NTFS matrix | Exit 101: **71 passed, 1 failed**, 0 ignored, no retries |
| Recipe completeness | **435 of 438** steps executed; the failed case stopped before its final three |
| Matrix timing | 1,524.6 s wall, runner 1,481.8 s (Pi: 3,737.3 s and 3,521.5 s) |
| VM reuse | The same Windows boot before and after the complete matrix |

[smoke-output.txt](smoke-output.txt), [smoke-repeat-output.txt](smoke-repeat-output.txt),
[smoke-rebased-output.txt](smoke-rebased-output.txt) and
[ntfs-output.txt](ntfs-output.txt) are the transcripts. Local paths are
replaced with `${CONSUMER}`, `${HARNESS}`, `${VM_STATE}` and similar; line
endings and trailing whitespace are normalized. Raw hashes and sizes are in
`results.json`.

### Matches the Linux baseline

Scenario by scenario, all 72 match the Linux report: status, executed and
expected steps, every chkdsk mode's state, and the Windows verdict's shape
and outcome. There are zero differences (`comparison_with_linux_2026_10_09`
in `results.json`). The same scenario failed at the same step, with the same
output:

```text
step 5 (mac-touch-index-allocation-rejected) exit Some(0) != expected 1
created file rec=298 //mac-rejected.txt
```

The Linux report's diagnosis stands. The pinned consumer supports
`$INDEX_ALLOCATION` insertion, so this is a stale negative expectation for
the consumer to resolve, not a harness or provider result.

The same four online scans reported an unavailable volume snapshot and used
the consumer's documented offline fallback. Each scenario passed, and every
read-only and offline check found no problems:

- `cli-windows-interrupted-index-vcn-fsck-replay-win-verify-chkdsk`
- `foreign-fragmented-indx-512-win-enumerate-chkdsk`
- `mac-format-tiny-32mib`
- `mac-format-volume-32mib-cluster-512`

The raw matrix log has **1,365 lines and 98,097 bytes**, over the unchanged
consumer budget of **1,300 lines / 90,000 bytes** (Pi: 1,444 / 107,771). The
scenario failure's exit 101 takes precedence; an otherwise passing run of
this size would fail the gate with exit 65. Neither the budget nor the
expectation was changed.


### Defects found and fixed on macOS

Each fix has a regression test that was red before it.

1. **The VirtIO ISO lists the ARM64 driver as hard links.** libarchive
   reports `NetKVM/w11/ARM64/*` as links into `NetKVM/2k25/...`, so
   extracting only that directory produced self-links or missing targets.
   `prepare` now extracts the whole `NetKVM` tree into a scratch directory
   and copies the ARM64 driver from it.
2. **The provider's own PowerShell leaked CLIXML.** `provision.log` held
   "Preparing modules for first use" progress records. Its commands now set
   the progress preference first, as the harness transport does.
3. **An idle-sleeping Mac froze the guest.** This MacBook sleeps after one
   idle minute on battery. The host slept during the first smoke run (339 s
   against 260 s for the repeat) and under both early shutdown attempts
   (`pmset` log). `wait`, `provision`, `down`, `exec` and `ssh` now hold an
   idle-sleep assertion for their duration. Closing the lid still sleeps.
4. **`down`'s ACPI power button left Windows dirty.** With the host awake,
   QEMU exited 4 s after `system_powerdown`, and the next boot recorded
   Kernel-Power 41 and EventLog 6008. A shutdown started inside Windows
   (`shutdown.exe /s /t 0` over SSH) logged User32 1074, EventLog 6006 and
   Kernel-Power 109, and the next boot was clean. `down` now does that, and
   falls back to the power button, with a warning, only when SSH fails. See
   [shutdown-events.txt](shutdown-events.txt). The Linux report did not
   exercise `down`, so whether KVM shares the power-button behaviour is
   unmeasured.
5. **The runner's dispatch tests shared scratch directories.** They are
   named by timestamp, and macOS's clock ticks in microseconds. Parallel
   tests collided and one test's cleanup deleted another's directory:
   `chore test` failed 2 runs in 6 at `c113f23` and 3 in 5 at the rebased
   head. Names now include the process id and a counter.

Ruff 0.16's default rules flag the existing provider code, so this
machine's commit hook refused any change to it. A behaviour-preserving
style commit comes first.

### Failed runs, retained

`chore check` run 06 failed only on a host dependency (`ModuleNotFoundError:
No module named 'jsonschema'`); run 08 passed with it. Run 60 printed 74
lines over its 70-line budget (defect 3's process handle and defect 4's
tests); runs 61 and 62 hit defect 5. `down` runs 14 and 15 timed out while
the host slept; a timeout leaves the guest running and kills nothing. Every
run is in `results.json`'s `commands`.

## Sign-off with the corrected consumer

rust-fs-ntfs#461 corrected the stale rejection (the insertion is now required
to succeed and Windows verifies it) and remeasured the matrix output budget at
1,850 lines / 135,000 bytes. It merged as `3e03fe2`, which also moves
`rust-fs-core` to 0.3.8. To sign this branch off without any virtio-fs code:

- Harness: this branch at `06cf970`, scp only -- before it was squashed onto
  `main` as `211edc4`, which brings `main`'s own recipe-reference fix (#46).
- Consumer: rust-fs-ntfs `main` at `3e03fe223c424418ce2a658eaeaa4175924a1312`,
  host tools rebuilt there; `rust-img-vhd` 0.6.0 rebuilt against
  `rust-fs-core` 0.3.8 as the consumer's CI does.
- A **fresh guest** in a new state directory, prepared, installed and
  provisioned by this branch: Windows setup 216 s after the one UEFI Shell
  interaction, `provision` 39 s, zero CLIXML in `provision.log`.

| Check | Result |
|---|---|
| `chore check` | All eight tasks pass; `agents-core` against the harness's pinned core (see below) |
| WinFsp smoke | Passed in 228 s: 18 positive steps, the canary failed as intended, zero CLIXML |
| Full NTFS matrix | **Exit 0: 72 passed, 0 failed, 438 of 438 steps, 0 skipped**, no retries |
| Timing | 1,859.6 s wall, runner 1,815.2 s |
| Output | 1,327 lines / 97,443 bytes, within the consumer's new budget |
| VM reuse | The same Windows boot before and after the matrix |

Four online scans still used the consumer's documented offline fallback.
Scenario by scenario -- status, steps, chkdsk mode states and verdicts -- the
result is identical to two earlier scp runs of the same consumer content on
the first guest. Transcripts: [signoff-ntfs-output.txt](signoff-ntfs-output.txt)
(raw SHA-256 `f2de0c98…2e54441`) and
[signoff-smoke-output.txt](signoff-smoke-output.txt); per-scenario results
in [signoff-results.json](signoff-results.json).

`agents-core` failed once in this layout because the consumer and harness
share one `../rust-fs-core` sibling: rust-fs-core 0.3.8 carries a newer
canonical shared block than `06cf970`'s `AGENTS.md`, which pinned 0.3.3. Run
against the pinned 0.3.3 it passed. The squashed branch now pins 0.3.8 and
carries the new block, and passes against it.

## VMware comparison: not measured

The existing VMware Fusion guest (Windows 11 ARM64, 2 vCPUs, 4 GiB; the
same sizes) would not power on. `vmrun` returned `Error: The operation was
canceled`. Its log shows the disk lock `Virtual Disk.vmdk.lck/M10056.lck`
held by `vmware-vmx` process 13994 from the previous session. That log
stops at 2026-10-08T22:12:30Z, and the Mac rebooted at 22:13:16Z with the
guest still running. Clearing the stale lock would change the guest bundle,
so it was left for the owner. The one power-on attempt rotated Fusion's
`vmware*.log` files; nothing else in the bundle changed. **Direct provider
parity between QEMU and VMware remains unmeasured.**

With the owner's approval the stale lock file was later removed and the
guest started. It could not run this consumer: it still carried the old
`vhd_tool.exe` rather than `rust-img-vhd`, and with the pinned helper placed
on its PATH temporarily, Windows in it failed to attach VHDs intermittently
(`Mount-DiskImage` HRESULT `0x800703e3`). Attaching freshly created blank
VHDs one at a time failed 3 of 8 there and 0 of 8 on the QEMU guest with the
identical script. The guest is Windows 11 Pro 26200, not the evaluation
LTSC 26100, and the cause inside it is unknown. Windows `disk` Event 51 and
Filter Manager errors are not evidence of a fault: the healthy QEMU guest
logs them too whenever the matrix mounts deliberately damaged images. The
helper was removed and the guest shut down; a partly removed work directory
remains in it. The first attempt's later SSH failures were the reporter's
own: the host's key vault idle-locked mid-run. Given a reproducible QEMU
guest that passes the full matrix, **retiring the VMware guest is
recommended** rather than repairing it.

## Transfer cost and a virtiofs spike

Image shipping (`ship-to-vm` + `ship-to-host`) took 894 s of the matrix's
4,770 s of summed step time (18.7%). The 16 GiB ship took 170 s. Uncontended
scp of the same 256 MiB image took 2.4 s, against about 22 s inside the
matrix, so much of the per-ship cost there is harness lease overhead and
four scenarios sharing QEMU's user-mode network.

A separate spike tested the owner's macOS virtiofsd port (`cth/macos` at
`390cc88`) with QEMU's `memory-backend-shm` and `vhost-user-fs-pci`. It ran
on a copy-on-write overlay of this guest, with virtio-win's `viofs` driver
and service. Windows mounted the share, and hashes matched in both
directions:

| 16 GiB image | scp | virtiofs copy |
|---|---|---|
| Host to guest | 139.9 s | 45.7 s |
| Guest to host | 226.8 s | 35.5 s |

With `--cache never`, 40 of 40 read-after-write checks across the share saw
fresh data. Windows attached a VHD stored on the share. Small-block reads
straight from the share were slow (`Get-FileHash` of 16 GiB took 511 s), so
a share would feed a local copy, not replace it. This is evidence for a
possible follow-up, not a change on this branch; the harness transport
remains SSH/scp. Spike measurements are in `results.json` under
`virtiofs_spike`. A follow-up on `feat/qemu-virtiofs` measured full matrices:
virtio-fs cut shipping by about 81% but Windows intermittently failed to read
from the share, so scp stays the transport.

## Remaining gates

- **VMware:** the current guest cannot attach VHDs reliably, so provider
  parity stays unmeasured; retirement is recommended over repair.
- **Consumer:** resolved by rust-fs-ntfs#461; the corrected matrix passes
  72/72 on macOS (above) and on the Pi (its own evidence in that pull
  request). The original 71/1 runs remain evidence and are not relabelled.
- **Linux re-validation:** this branch replaces 7-Zip and genisoimage with
  `bsdtar` on both hosts (Debian: `libarchive-tools`), discovers firmware,
  and shuts Windows down over SSH. The Pi has not run fresh provisioning or
  `down` with these changes.
- **Recipe-reference fix:** measured -- the sign-off ran with `e152059`, and
  no NTFS scenario's result differs from the earlier scp runs.
- **Exact head:** the sign-off ran on `06cf970`; the squashed head carries
  `main`'s version of the recipe-reference fix, so the matrix is rerun there.

## Reproduction shape

Follow [the QEMU setup guide](../../qemu-vm.md). Commands, in order, are in
`results.json`'s `commands`:

```sh
python3 scripts/local-vm.py prepare --accept-evaluation-terms --media-dir "$MEDIA"
python3 scripts/local-vm.py up --install
python3 scripts/local-vm.py boot-installer      # at the UEFI Shell prompt
python3 scripts/local-vm.py wait --timeout 1800
python3 scripts/local-vm.py provision
python3 scripts/local-vm.py exec -- chore smoke --vm-host fswth-local --vm-workdir C:/fswth/smoke-consumer
```

The consumer side is unchanged from the Linux report: build its two host
tools, install its pinned Windows VHD helper in the guest, set the ignored
`.test-env` to `VM_HOST=fswth-local`, an empty `SSH_KEY` and a separate work
directory, then from the consumer checkout:

```sh
python3 "$HARNESS/scripts/local-vm.py" exec -- \
  bash ../rust-fs-core/scripts/tier.sh --refuse-skips --refuse-ignored \
  matrix -- bash scripts/run-matrix.sh
python3 "$HARNESS/scripts/local-vm.py" exec -- bash scripts/matrix-fetch-diag.sh
python3 "$HARNESS/scripts/local-vm.py" exec -- bash scripts/vm-clean-images.sh
```

No VM reset, selective filter, retry, changed concurrency or changed timeout
was used to obtain these results.
