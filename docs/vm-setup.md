# Setting up the Windows VM

For the persistent **Linux ARM64 QEMU/KVM** setup, including Raspberry Pi 5,
use [qemu-vm.md](qemu-vm.md). This page retains the VMware setup while the
replacement is validated. macOS QEMU support is a subsequent step.

Everything the harness needs from the Windows side, from an empty
hypervisor to `ssh <user>@<vm> "echo ok"` answering. Do this once per
machine. The rest of the harness assumes it is done: `run-tests.sh` talks to
whatever `VM_HOST` in the consumer's `.test-env` names, and a VM it cannot
reach reads as a wall of red scenarios, not as "the VM is not set up".

Written against **VMware Fusion 13 on an Apple-silicon Mac, with a Windows 11
ARM guest**, which is the setup the harness is used with. The Windows half
(sections 4–6) is the same on any hypervisor. For others, the idea of each
hypervisor step carries over; only the menu names differ.

## 1. The shape we are building

```
   Mac                                        Windows VM
   ─────────────────────────                  ────────────────────────────
   vmnet8 (NAT, DHCP)  192.168.x.1  ◄────────► Ethernet   (DHCP, internet)
   vmnetN (host-only)  10.254.254.1 ◄────────► Ethernet 2 10.254.254.253 (static)
                                                 └─ sshd, Private profile
```

**Two network adapters, for two jobs.**

- **NAT** is how Windows reaches the internet (Windows Update, winget,
  rustup). Its address comes from Fusion's DHCP, and **it moves**: one VM
  here was `.146` for months and came back as `.150` after a restart. A test
  config that names that address breaks without warning, and SSH's
  `known_hosts` breaks with it.
- **Host-only, with a static address**, is how the Mac reaches Windows. Only
  the Mac and the VM are on it. Nothing hands out addresses, so the
  address is the one you typed and never changes. **`VM_HOST` always names
  this one.**

`10.254.254.0/24` is used here because nothing else on a normal home or
office network uses it. Any private subnet that does not overlap one the
Mac already routes will do. Pick one and use it everywhere below.

## 2. VMware: the host-only network

Fusion → **Settings… → Network → +** (unlock with your admin password), and
on the new network (Fusion names it, e.g. `vmnet2`):

| setting | value |
|---|---|
| Allow virtual machines on this network to connect to external networks (using NAT) | **off** |
| Connect the host Mac to this network | **on** |
| Provide addresses on this network via DHCP | **off** |
| Subnet IP | `10.254.254.0` |
| Subnet Mask | `255.255.255.0` |

Apply. The Mac side of it is `10.254.254.1`. Fusion assigns that itself;
check it with `ifconfig | grep 10.254.254`.

## 3. VMware: the VM

1. **Shut Windows down** (from inside Windows, or `vmrun … stop <vmx> soft`).
2. VM **Settings → Add Device… → Network Adapter**, attached to the network
   from section 2. Leave the existing adapter on **Share with my Mac** (NAT).
3. Start the VM.

### Encryption and the password

Windows 11 needs a TPM, and Fusion's virtual TPM **requires the VM to be
encrypted**. Fusion's default is *partial* encryption, which covers the
config and TPM state, not the virtual disk. You cannot remove the encryption
while the TPM is present. Removing the TPM destroys whatever Windows sealed
into it, including a BitLocker key, which makes a BitLocker-protected disk
unreadable. Leave it encrypted.

So every `vmrun` call that opens the VM needs the encryption password,
passed as `-vp`:

```sh
vmrun -T fusion -vp "$PASSWORD" start "<path>.vmx" nogui
vmrun -T fusion -vp "$PASSWORD" stop  "<path>.vmx" soft
vmrun -T fusion -vp "$PASSWORD" getGuestIPAddress "<path>.vmx" -wait
vmrun list                                    # no password needed
```

(`vmrun` is at `/Applications/VMware Fusion.app/Contents/Public/vmrun`.)

- **Where the password is.** If you never type one when opening the VM,
  Fusion generated it and saved it in the macOS login keychain. The
  keychain entry's service name is the `.vmx` path. Keep it in your secret
  store and have scripts read it from there, never from a file in a
  repository. For example, copying it from the keychain without it being
  displayed:

  ```sh
  security find-generic-password -w -s "<path>.vmx" \
    | trove add password <entry> --secret-stdin
  ```
- **The trade-off.** `-vp` puts the password on `vmrun`'s command line for
  as long as that call runs. vmrun has no stdin or file option, so that is
  the only way to pass it.

### Headless, and what the Fusion window does not tell you

`vmrun … start … nogui` runs the VM as a background process
(`vmware-vmx`) with no window attached.

- **The Fusion app is only a viewer.** Its window can look idle or powered
  off while Windows is running.
- **Quitting Fusion does not stop the VM.** Stop it with `vmrun … stop`.
- **Opening the VM in Fusion attaches to the running VM**, without
  restarting it.

`vmrun list` and an SSH probe are the source of truth, not the window.

## 4. Windows: the static address

In an **administrator** PowerShell in the VM, first find the adapter.
The new one is the one with no address and a MAC address different from
the NAT adapter's:

```powershell
Get-NetAdapter | Format-Table Name, InterfaceDescription, MacAddress, Status
```

Then, with its name (typically `Ethernet 2`):

```powershell
$if = "Ethernet 2"
New-NetIPAddress -InterfaceAlias $if -IPAddress 10.254.254.253 -PrefixLength 24
# NO -DefaultGateway: the internet goes out through the NAT adapter, and a
# second default route would split traffic between the two.

# A network with no gateway is an "Unidentified network", which Windows puts
# in the PUBLIC firewall profile -- and the sshd firewall rule is enabled for
# PRIVATE only. Without this line SSH answers on the NAT address and is
# silently dropped on this one.
Set-NetConnectionProfile -InterfaceAlias $if -NetworkCategory Private
```

Check from the Mac: `ping 10.254.254.253`.

## 5. Windows: OpenSSH server

`scripts/setup-windows-vm.ps1` provisions the toolchain and packages but
**does not install sshd**, and it needs SSH to run. So this comes first, in
an administrator PowerShell:

```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service sshd -StartupType Automatic
Start-Service sshd

# The harness sends PowerShell commands, so make it the login shell.
New-ItemProperty -Path HKLM:\SOFTWARE\OpenSSH -Name DefaultShell `
  -Value "$env:SystemRoot\System32\WindowsPowerShell\v1.0\powershell.exe" `
  -PropertyType String -Force
```

The capability install creates the firewall rule
`OpenSSH SSH Server (sshd)`. Check that it covers the Private profile:
`Get-NetFirewallRule -DisplayName '*sshd*' | Format-Table DisplayName, Enabled, Profile`.

**Key login.** Where the public key goes depends on whether the account is
an administrator. This is Windows' rule, and the usual reason key login
"doesn't work":

- **administrator account:** `C:\ProgramData\ssh\administrators_authorized_keys`,
  readable only by Administrators and SYSTEM:

  ```powershell
  Add-Content C:\ProgramData\ssh\administrators_authorized_keys "<public key>"
  icacls C:\ProgramData\ssh\administrators_authorized_keys /inheritance:r /grant "Administrators:F" /grant "SYSTEM:F"
  ```
- **standard account:** `C:\Users\<user>\.ssh\authorized_keys`.

**On the Mac.** If the private key is in an SSH agent (a secret store's
agent, or `ssh-add`), `ssh` offers every key the agent holds. More than
about five keys and the server hangs up (`Too many authentication
failures`) before reaching the right one. Name the key: save its **public**
half and pass that with `IdentitiesOnly`. ssh then asks the agent for just
that key, and no private key is written to disk:

```sh
ssh-add -L | awk '$3 == "<key comment>"' > ~/.ssh/fs-windows-vm.pub
ssh -o IdentitiesOnly=yes -i ~/.ssh/fs-windows-vm.pub <user>@10.254.254.253 "echo ok"
```

The first connection records the VM's host key in `~/.ssh/known_hosts`.
Because the address is fixed, it stays valid.

## 6. The consumer's `.test-env`

Each consumer keeps its machine-specific VM settings in a gitignored
`.test-env` at its root. `run-tests.sh` writes it on the first run from
prompts, or you write it yourself:

```sh
VM_HOST=<user>@10.254.254.253       # the host-only address, never the NAT one
SSH_KEY=/Users/<you>/.ssh/fs-windows-vm.pub
VM_WORKDIR=C:/Users/<user>/dev/<consumer>-matrix
HOST_IMAGE_DIR=/tmp
```

A consumer that starts and stops the VM itself (for example from a chore
task) adds the `.vmx` path there as well:
`VM_VMX=/Users/<you>/Virtual Machines.localized/<name>.vmwarevm/<name>.vmx`.

Then provision the toolchain with `scripts/setup-windows-vm.ps1` (see the
header of that script for its arguments).

## 7. When the VM misbehaves

- **Slow, or the window shows nothing, right after a long gap.** Windows is
  installing the updates it missed. You'll see `TiWorker` (Windows Modules
  Installer) at the top of CPU, and `wuauserv` and `TrustedInstaller`
  running. Let it finish; it may restart itself. A restart request
  (`shutdown /r /f /t 0`) can be held off by the installer for minutes.
- **Hard reset** (`vmrun … reset <vmx> hard`) is pulling the plug. It
  works when the guest won't shut down. Mid-update, Windows then rolls the
  update back on the next boot, which is recoverable but slow. Prefer
  `stop … soft` when the guest is responsive.
- **"Host key verification failed"** means the address changed and
  `known_hosts` has no key for the new one. With the host-only address from
  section 4 this does not happen. If `VM_HOST` still names the NAT address,
  that is the fix.
- **SSH answers on one address and not the other.** Check the firewall
  profile of the adapter that doesn't answer (section 4,
  `Set-NetConnectionProfile`).
