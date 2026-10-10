# Persistent Windows ARM64 VM with QEMU

One setup for two hosts: Linux ARM64 with KVM (including Raspberry Pi 5) and
Apple Silicon macOS with Hypervisor.framework (HVF). Both use the same tools,
commands, media, state layout, guest and SSH transport; the accelerator is the
only per-host difference, chosen automatically. The scenario runner, recipes,
Windows operations, assertions and CI Windows runner remain the existing
harness. This is a guest provider: prepare once, start when needed, run many
matrices, and shut down explicitly. Tests do not reboot the VM.

## Prerequisites

Every host needs Python 3.11+, QEMU with `qemu-img`, ARM UEFI firmware, an
OpenSSH client, curl and `bsdtar` (libarchive, which both extracts the VirtIO
driver and writes the seed ISO). Only the install command differs:

| Host | Accelerator | Install |
|---|---|---|
| Debian / Raspberry Pi OS, ARM64 | KVM: read/write access to `/dev/kvm` | `sudo apt install qemu-system-arm qemu-utils qemu-efi-aarch64 openssh-client curl libarchive-tools python3` |
| macOS, Apple Silicon | HVF: `sysctl kern.hv_support` is 1 | `brew install qemu python` (`bsdtar`, curl and ssh ship with macOS) |

macOS's `/usr/bin/python3` is 3.9 and is refused by name: run the commands
with Homebrew's `python3` (put `$(brew --prefix)/bin` first on `PATH`). No
host root access is needed after dependencies and KVM permissions are
configured; HVF needs none.

`prepare` finds the firmware itself: Debian's `qemu-efi-aarch64`
(`/usr/share/AAVMF/AAVMF_CODE.fd` + `AAVMF_VARS.fd`) first, then QEMU's own
build beside the `qemu-system-aarch64` on `PATH`
(`share/qemu/edk2-aarch64-code.fd` + `edk2-arm-vars.fd`, as Homebrew installs
it). `--firmware-code` and `--firmware-vars` override both and must be passed
together. The chosen paths and their SHA-256 are recorded in `vm.json` and
copied into the state directory, so a later QEMU upgrade does not change an
installed guest's firmware.

Resources: 2 virtual CPUs, 4 GiB RAM, about 6 GiB of cached media and a sparse
128 GiB guest disk, on every host. Large matrices also need room for host
images. Run one matrix at a time and choose consumer concurrency to fit the
guest memory; multiple drive letters do not guarantee capacity.

## Windows evaluation terms

Read Microsoft's [evaluation instructions and terms](https://www.microsoft.com/en-us/evalcenter/download-windows-11-iot-enterprise-ltsc-eval)
and register as requested there before downloading. The pinned ISO is Windows
11 IoT Enterprise LTSC 2024 Evaluation, ARM64, build 26100.1742. It has a
90-day evaluation period; normal online activation needs no product key.
Downloading it again does **not** establish a renewed licence or permission
for indefinite rolling evaluations. This tool provides no renewal, rearm or
scheduled reinstall. Activation and expiry are recorded in `provision.log`.

`scripts/local-vm/media.json` records exact sizes, SHA-256 hashes and HTTPS
sources. A changed download fails closed. Microsoft's hash PDF covers an older
ISO; the manifest explains the refreshed ISO hash's provenance. The pinned
OpenSSH ARM64 package is Microsoft's `10.0.0.0p2-Preview` release. WinFsp's MSI
signature is checked inside Windows before installation.

## Prepare and install

Run from the harness checkout:

```sh
python3 scripts/local-vm.py prepare --accept-evaluation-terms
python3 scripts/local-vm.py up --install
python3 scripts/local-vm.py screenshot /tmp/fswth-install.png
```

Default state is `~/.local/share/fs-windows-test-harness/arm64`. To select
another instance, put `--state /absolute/path` **before** the subcommand on
every invocation. `prepare --ssh-port 22261` selects a different loopback
port; default is 22260. `--media-dir /path/to/cache` reuses downloads while
still verifying every hash. Firmware can be selected with `--firmware-code`
and `--firmware-vars`. State paths may contain spaces, but not commas, double
quotes, percent signs or newlines.

There is one initial firmware interaction. If the screenshot shows the
installer's **press any key** prompt, use `key spc`. If it shows **UEFI
Interactive Shell / Shell>**, use:

```sh
python3 scripts/local-vm.py boot-installer
```

This enters `fs0:\efi\boot\bootaa64.efi` and presses space. Only use it at
that initial shell prompt. Subsequent Windows setup is unattended. Take
another screenshot to confirm it has started, then:

```sh
python3 scripts/local-vm.py wait --timeout 1800
python3 scripts/local-vm.py provision
```

The answer file formats **this new guest's disk 0**. No host disks or directories
are passed through. An already prepared disk cannot be prepared again, and
`up --install` is refused after its first launch. If interrupted after Windows
has been copied, use ordinary `up` to boot the installed disk. A failed first
launch or an unusable partial installation needs a separately named state
directory; inspect `qemu.log` before retrying anything.

The first PowerShell session prepares modules and can take tens of seconds
on a Pi. Each readiness probe allows up to 60 seconds, bounded by the total
wait; `provision` allows five minutes for readiness before starting its work.

SSH/network setup runs during Windows specialize. WinFsp installation runs
after setup, because installing its MSI during specialize stalled in the
prototype. `provision` sets RemoteSigned for the dedicated account, performs
normal evaluation activation, installs WinFsp (including developer files),
disables AC idle sleep, and ejects both installer discs. This base guest does
not install every consumer's build dependencies: provision its Rust toolchain,
compiler and helper programs according to that consumer's setup instructions.
IoT evaluation media may lack `winget`; verify that prerequisite before using
a consumer's winget-based installer.

## Run the harness

```sh
python3 scripts/local-vm.py up
python3 scripts/local-vm.py wait
python3 scripts/local-vm.py exec -- chore smoke \
    --vm-host fswth-local --vm-workdir C:/fswth/smoke-consumer

# Repeat: same VM, same installed Windows, all smoke assertions run again.
python3 scripts/local-vm.py exec -- chore smoke \
    --vm-host fswth-local --vm-workdir C:/fswth/smoke-consumer
```

Run ordinary consumer harness commands inside `exec --` too. It preserves
the current working directory and exit status, and supplies `ssh`/`scp`
wrappers with the instance's SSH config. Set `--vm-host=fswth-local` and a
separate `--vm-workdir=C:/fswth/<consumer>` for each consumer. The wrapper's
config supplies the private key; no `--ssh-key` argument is needed. An existing
`.test-env` that names another key must be updated (or reset in a disposable
consumer worktree). A consumer task that explicitly invokes VMware still needs
to call this provider's `up` instead; the harness itself never calls VMware.

`exec` holds a local lock for the complete command, preventing another wrapped
run, provisioning operation or shutdown from using the same guest concurrently.
The existing remote workdir lease remains in force. Commands run outside this
wrapper are not covered by its local lock.

The existing `--no-ship` flag can reuse a known unchanged deployment. Do not
use it after changing source, scripts, build settings or tools; this branch
does not implement automatic deployment fingerprinting.

## Inspect and stop

```sh
python3 scripts/local-vm.py status
python3 scripts/local-vm.py ssh -- 'Get-CimInstance Win32_OperatingSystem | Select-Object Caption,LastBootUpTime'
python3 scripts/local-vm.py ssh -- 'cscript.exe //nologo C:\Windows\System32\slmgr.vbs /xpr'
python3 scripts/local-vm.py down
```

`down` asks Windows to shut down (`shutdown.exe /s /t 0` over the guest's
SSH) and waits for QEMU to exit; it is bounded, a timeout is an error and
leaves the guest running, and no forced kill is hidden behind it. Only when
SSH fails does it press the ACPI power button instead, with a warning: on
HVF that path powered the guest off but Windows recorded an unexpected
shutdown (Kernel-Power 41, EventLog 6008) on its next boot, while its own
shutdown booted clean. QEMU is daemonized, with no system service or
autostart installed. A host restart or stopping its owning service can stop
it; ordinary `up` restarts the persistent disk.

A Mac idle-sleeps on a timer, and a sleeping host freezes the guest. On macOS,
`wait`, `provision`, `down`, `exec` and `ssh` therefore hold an idle-sleep
assertion (`caffeinate -i`) until they exit. Closing the lid still sleeps the
Mac, and a guest left running between commands is frozen while it sleeps:
keep the lid open (on power, for long matrices) while a run is in progress.

State is private to the host account: SSH key, generated local Windows
administrator password, unattended seed and guest disk are never repository
files. SSH forwards **127.0.0.1 only**. QMP and VNC use private Unix sockets.
Windows logs are in `C:\fswth-bootstrap`; host logs are `qemu.log`,
`serial.log` and `provision.log` in the state directory. Initial SSH host-key
trust is scoped to that private instance; later key changes are refused.

## Acceptance

`chore local-vm` tests lifecycle decisions, download integrity, locking and
transport without a guest. `chore check` and CI include it. These checks do
not prove Windows behavior. Acceptance requires fresh provisioning through
this branch and a complete real filesystem consumer matrix, with every
scenario accounted for and its diagnostics retained. VMware replacement and
macOS compatibility are separate claims requiring their own evidence.

The [2026-10-09 Pi validation](validation/qemu-ntfs-2026-10-09/README.md)
completed fresh provisioning and WinFsp smoke, then ran the unchanged full
NTFS consumer: 71 scenarios passed and one failed an expected-rejection
assertion. Four online scans used offline fallbacks, and the transcript
exceeded the consumer's budget. Full parity is not yet accepted; that report
retains the evidence and the remaining gates. No NTFS source or tests were
changed.

The [2026-10-10 macOS validation](validation/qemu-ntfs-macos-2026-10-10/README.md)
did the same on an Apple Silicon MacBook with HVF: fresh provisioning,
smoke twice on one Windows boot, a clean shutdown and restart, and the same
unchanged NTFS consumer. All 72 scenarios match the Pi run, the same failure
and four fallbacks included. Its VMware comparison was not measured.

## MacBook, Pi and VMware retirement

A MacBook runs the same provider locally: same commands, same state layout,
HVF in place of KVM. Install its dependencies as in Prerequisites and follow
the same steps. Keep the lid open during long runs.

To offload test work to a Pi instead, SSH from the MacBook into it and run the
existing `local-vm.py exec --` commands there. This keeps the host tool build,
image work and Windows guest on the Pi and preserves the provider lock.
The Pi must have the intended consumer revision and fixtures available;
verify its commit before running.

A MacBook can already use the ordinary SSH-based harness against the Pi's
running Windows guest. The guest SSH port is bound to the Pi's loopback:
use a secured SSH tunnel to that port, with the guest key kept private and
host-key checking enabled. Run the consumer's host tools on the MacBook and
use a distinct Windows work directory. The Linux provider's local lock does
not cover commands issued remotely from the MacBook; serialize runs through
the Pi or otherwise ensure only one run uses that guest. Do not expose the
Windows SSH port publicly to make it reachable.

Before deprecating VMware, retain evidence for these checks:

1. Fresh Windows installation and provisioning on each supported host,
   followed by the complete smoke test, including its expected failing canary.
2. Repeat runs reuse the same Windows boot, and shutdown/restart retains the
   guest installation. Record the host, guest, helper and harness versions.
3. Run the same pinned consumer revision, scenarios, recipes and timeouts on
   both providers. At minimum compare the failed expected-rejection scenario
   and the four online-scan fallback scenarios from the Pi report; compare
   the complete matrix to establish full coverage equivalence.
4. Resolve the consumer's stale expected rejection and remeasure its output
   budget in that repository, then run the complete corrected matrix on Linux
   and macOS. The original failed run remains evidence and is not relabelled.
5. Preserve all step diagnostics and hashes of the images used for supplemental
   investigations. Verify any lifecycle calls in consumer task definitions:
   some still invoke VMware even though the harness transport uses SSH.

A successful QEMU run demonstrates that provider's behavior. A matching
VMware run establishes the comparison. Native Windows CI remains a third
execution environment using the same assertions and needs no local VM.
