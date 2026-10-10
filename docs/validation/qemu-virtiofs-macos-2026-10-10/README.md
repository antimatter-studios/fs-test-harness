# scp versus virtio-fs image shipping on macOS — 2026-10-10

**virtio-fs cut image shipping by about 81% and scenario time by about 27%,
but it is not reliable: both virtio-fs runs failed 3 of 72 scenarios with a
Windows read error on the share.** Both scp runs passed all 72. scp stays the
default; the share remains an opt-in experiment until that error is
understood.

## Inputs

- Harness: `feat/qemu-virtiofs` at `3847a18`, identical for every run.
- Consumer: rust-fs-ntfs pull request #461 head
  `1719a7bcabf57363b8f0a110e9e3fde23eed859c`, which is `fe450f1` plus two
  test-only commits correcting the stale index-allocation expectation and
  the output budget. The host driver binaries are byte-identical to the
  `fe450f1` build (`rust-ntfs` `afc2126d…`).
- Guest: the macOS validation guest (Windows 11 IoT Enterprise LTSC 2024
  Evaluation ARM64, 2 vCPUs, 4 GiB), with `share-driver` installed once.
  virtio-win 0.1.302 `viofs` and `virtiofs.exe`, WinFsp 2.1.25156.
- Host: MacBook Pro M3 Pro on AC power, QEMU 10.2.2 (HVF), the owner's macOS
  virtiofsd port (`cth/macos` at `390cc88`) with `--cache never`.
- Runs alternated scp, virtio-fs, scp, virtio-fs, each from a fresh Windows
  boot with the same work directory and the same host image directory; only
  the mode differed ([run-timed.sh](run-timed.sh)). The host carried a
  desktop load average of about 6–15 throughout, which the alternation
  spreads across both modes.

## Results

| Run | Wall | Runner | Shipping | Passed | Steps |
|---|---|---|---|---|---|
| scp 1 | 1,915.0 s | 1,894.8 s | 1,122 s | **72 / 72** | 438 / 438 |
| virtio-fs 1 | 1,457.3 s | 1,444.6 s | 195 s | 69 / 72 | 429 / 438 |
| scp 2 | 2,057.9 s | 2,043.2 s | 1,289 s | **72 / 72** | 438 / 438 |
| virtio-fs 2 | 1,352.0 s | 1,338.5 s | 189 s | 69 / 72 | 433 / 438 |

Both scp runs exited 0: every scenario passed, including the corrected
index-allocation insertion case, and the output stayed within the pull
request's new budget (1,329 and 1,334 lines). Four online scans still used
the consumer's documented offline fallback.

The failing virtio-fs scenarios stopped early, so the raw wall times
overstate the gain. Over the 67 scenarios that passed in all four runs
([comparison-common-scenarios.json](comparison-common-scenarios.json)):

| Mean of two runs | scp | virtio-fs | Saving |
|---|---|---|---|
| Shipping | 902.5 s | 166.7 s | 81.5% |
| Step time | 6,175.5 s | 4,929.3 s | 20.2% |
| Scenario time | 6,827.8 s | 4,986.8 s | 27.0% |

Scenario time also includes each scenario's lease and cleanup overhead, which
the scp path spends more of.

## The read failure

Every virtio-fs failure was a `ship-to-vm` copy from the share:

```text
Copy-Item : Error performing inpage operation.
```

Run 1 failed `cli-windows-6-grow-mft-win-verify-chkdsk`,
`mac-format-volume-16gib-cluster-4k` and
`mac-format-win-write-rename-win-chkdsk`; run 2 failed
`mac-format-large-1gib`, `mac-format-mac-split-index-root-win-chkdsk` and
`mac-format-volume-16gib-cluster-4k`. The same scenarios passed over scp.

A reproduction outside the matrix copied one 16 GiB and three 1 GiB NTFS
images, built with the matrix's own recipes, from the share:

| Copy | Failures |
|---|---|
| Four at once, `Copy-Item` (buffered) | 3 of 12 |
| Four at once, `robocopy /J` (unbuffered) | 2 of 12 |
| One at a time, `Copy-Item` | 3 of 8 |

So it is neither concurrency nor the Windows cache manager alone. With
`--log-level debug`, virtiofsd answered **all 155,220 read requests** in the
reproduction, every one a full 256 KiB, with no error replies and none left
unanswered. Windows' event logs record nothing beyond the service starting.
The failure therefore lies on the guest side of the reply -- the virtio-win
`virtiofs.exe` client and `viofs` driver on WinFsp -- or in the vhost-user
transport between virtiofsd and QEMU on macOS. This evidence does not say
which. An earlier spike copied single 16 GiB and 256 MiB files successfully,
which shows the failure is intermittent rather than size-bound.

## Next steps to isolate it

- The same guest on Linux KVM with upstream virtiofsd: a clean run there
  points at the macOS transport or port, a failure at the Windows client.
- A newer virtio-win `viofs` and `virtiofs.exe` on macOS.
- `virtiofs.exe` debug output in the guest during a failing copy.

## VMware comparison attempts

With the owner's approval the VMware guest's stale disk lock was removed and
the pinned `rust-img-vhd` placed on its PATH temporarily. The guest could not
run the matrix: it still lacked the current helper by default, and Windows
in it failed to attach VHDs intermittently (`Mount-DiskImage` HRESULT
`0x800703e3`), 3 of 8 freshly created blank VHDs one at a time against 8 of
8 on the QEMU guest with the identical script. The first attempt's later
failures were the reporter's own: the host's key vault locked mid-run. The
cause inside the VMware guest is unknown. Windows `disk` Event 51 and Filter
Manager errors are not evidence of a fault there: the healthy QEMU guest
logs them too whenever the matrix mounts deliberately damaged images. The
helper files were removed and the guest shut down afterwards; a partly
removed work directory remains in it.
