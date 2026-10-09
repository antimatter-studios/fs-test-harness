#!/usr/bin/env python3
"""Persistent Windows ARM64 evaluation VM for Linux ARM64/KVM and Apple
Silicon macOS/HVF (Python 3.11+).

VM lifecycle is explicit. Running a harness command leaves the guest running.
State, media, credentials and logs stay outside the checkout. Both hosts use
the same tools, commands and state layout; only the accelerator differs.
"""

import argparse
import base64
import contextlib
import fcntl
import hashlib
import json
import os
import platform
import secrets
import shlex
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from xml.sax.saxutils import escape

RESOURCES = Path(__file__).resolve().with_suffix("")
DEFAULT_STATE = Path.home() / ".local/share/fs-windows-test-harness/arm64"
# The accelerator is the only per-host difference. gic-version=host requires
# KVM, so HVF uses QEMU's emulated GICv3 (measured: QEMU 10.2.2, Apple M3 Pro).
HOSTS = {
    ("Linux", "aarch64"): "kvm",
    ("Linux", "arm64"): "kvm",
    ("Darwin", "arm64"): "hvf",
}
GIC = {"kvm": "host", "hvf": "3"}
# bsdtar (libarchive) reads and writes ISO 9660 on both hosts: built into
# macOS, libarchive-tools on Debian.
TOOLS = (
    "qemu-system-aarch64",
    "qemu-img",
    "ssh-keygen",
    "ssh",
    "scp",
    "bsdtar",
    "curl",
)
# Debian/Raspberry Pi OS qemu-efi-aarch64, checked before QEMU's own build.
SYSTEM_FIRMWARE = (
    Path("/usr/share/AAVMF/AAVMF_CODE.fd"),
    Path("/usr/share/AAVMF/AAVMF_VARS.fd"),
)


def run(argv, **kwargs):
    return subprocess.run([str(x) for x in argv], check=True, **kwargs)


def write_json(path, value):
    temp = path.with_suffix(".tmp")
    temp.write_text(json.dumps(value, indent=2) + "\n")
    temp.replace(path)


def tool(name):
    found = shutil.which(name)
    if not found:
        raise RuntimeError(f"Missing {name}; see docs/qemu-vm.md prerequisites")
    return found


def private_dir(path):
    path.mkdir(parents=True, mode=0o700, exist_ok=True)
    if path.stat().st_uid != os.getuid():
        raise RuntimeError(f"Directory is owned by another user: {path}")
    path.chmod(0o700)


def runtime_dir(state):
    # sun_path is 108 bytes on Linux, 104 on macOS; keep sockets independent
    # of long state paths.
    parent = Path(f"/tmp/fswth-{os.getuid()}")
    private_dir(parent)
    path = parent / hashlib.sha256(os.fsencode(state)).hexdigest()[:16]
    private_dir(path)
    return path


@contextlib.contextmanager
def exclusive(state):
    private_dir(state)
    with (state / "operation.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as exc:
            raise RuntimeError(
                "Another lifecycle operation or test run owns this VM"
            ) from exc
        yield


def qmp(state, command, arguments=None):
    with socket.socket(socket.AF_UNIX) as sock:
        sock.settimeout(10)
        sock.connect(str(runtime_dir(state) / "qmp.sock"))
        with sock.makefile("rwb", buffering=0) as stream:
            if "QMP" not in json.loads(stream.readline()):
                raise RuntimeError("Unexpected QMP greeting")
            for ident, name, args in (
                (1, "qmp_capabilities", {}),
                (2, command, arguments or {}),
            ):
                stream.write(
                    (
                        json.dumps({"execute": name, "arguments": args, "id": ident})
                        + "\n"
                    ).encode()
                )
                deadline = time.monotonic() + 10
                while True:
                    if time.monotonic() >= deadline:
                        raise TimeoutError(f"QMP {name} timed out")
                    line = stream.readline()
                    if not line:
                        raise RuntimeError("QMP closed before responding")
                    result = json.loads(line)
                    if result.get("id") == ident:
                        break
                if "error" in result:
                    raise RuntimeError(f"QMP {name}: {result['error']}")
            return result["return"]


def running(state):
    try:
        return qmp(state, "query-status")
    except (FileNotFoundError, ConnectionRefusedError):
        return None


def verify(path, spec):
    if not path.is_file() or path.stat().st_size != spec["bytes"]:
        return False
    return sha256(path) == spec["sha256"]


def fetch(media, manifest):
    private_dir(media)
    for name, spec in manifest.items():
        dest = media / name
        if dest.exists():
            if not verify(dest, spec):
                raise RuntimeError(
                    f"Cached media does not match pinned hash/size: {dest}"
                )
        else:
            part = dest.with_suffix(dest.suffix + ".part")
            print(f"Downloading {name}", flush=True)
            run(
                [
                    tool("curl"),
                    "--fail",
                    "--location",
                    "--silent",
                    "--show-error",
                    "--retry",
                    "2",
                    "--connect-timeout",
                    "20",
                    "--max-time",
                    "1800",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--output",
                    part,
                    spec["url"],
                ],
                timeout=5500,
            )
            if not verify(part, spec):
                raise RuntimeError(f"Download hash/size mismatch: {part}; not used")
            part.replace(dest)
        print(f"Verified {name}", flush=True)


def render_answer(password):
    return (
        (RESOURCES / "Autounattend.xml.in")
        .read_text()
        .replace("@PASSWORD@", escape(password))
    )


def ssh_config(state, port):
    return f'''Host fswth-local
    HostName 127.0.0.1
    Port {port}
    User fswth
    IdentityFile "{state / "ssh-key"}"
    IdentitiesOnly yes
    BatchMode yes
    ConnectTimeout 5
    ServerAliveInterval 15
    ServerAliveCountMax 3
    StrictHostKeyChecking accept-new
    UserKnownHostsFile "{state / "known_hosts"}"
'''


def transport(state, port):
    (state / "ssh_config").write_text(ssh_config(state, port))
    private_dir(state / "bin")
    # Both bash and the Rust runner execute these. SSH_OPTS alone does not
    # carry the forwarded port through the Rust runner's SSH/scp calls.
    for name in ("ssh", "scp"):
        executable = tool(name)
        wrapper = state / "bin" / name
        wrapper.write_text(
            f'#!/bin/sh\nexec {shlex.quote(executable)} -F {shlex.quote(str(state / "ssh_config"))} "$@"\n'
        )
        wrapper.chmod(0o700)


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def accelerator():
    system, machine = platform.system(), platform.machine().lower()
    accel = HOSTS.get((system, machine))
    if accel is None:
        raise RuntimeError(
            f"Unsupported host {system} {machine}; needs Linux ARM64 with KVM or an Apple Silicon Mac"
        )
    if accel == "kvm" and not os.access("/dev/kvm", os.R_OK | os.W_OK):
        raise RuntimeError(
            "/dev/kvm is not accessible; enable KVM and grant this user access"
        )
    if accel == "hvf":
        result = subprocess.run(
            ["sysctl", "-n", "kern.hv_support"],
            capture_output=True,
            text=True,
            check=False,
        )
        if result.stdout.strip() != "1":
            raise RuntimeError(
                "Hypervisor.framework is unavailable (sysctl kern.hv_support is not 1)"
            )
    return accel


def firmware(args):
    if args.firmware_code or args.firmware_vars:
        if not (args.firmware_code and args.firmware_vars):
            raise RuntimeError("Pass both --firmware-code and --firmware-vars")
        candidates = [(Path(args.firmware_code), Path(args.firmware_vars))]
    else:
        # QEMU's bundled edk2 build (Homebrew, MacPorts, source installs):
        # its aarch64 code shares the ARM variable-store template.
        share = Path(tool("qemu-system-aarch64")).resolve().parents[1] / "share/qemu"
        candidates = [
            SYSTEM_FIRMWARE,
            (share / "edk2-aarch64-code.fd", share / "edk2-arm-vars.fd"),
        ]
    for code, variables in candidates:
        if code.is_file() and variables.is_file():
            return code.resolve(), variables.resolve()
    tried = "; ".join(f"{code} + {variables}" for code, variables in candidates)
    raise RuntimeError(
        f"Missing ARM UEFI firmware (tried {tried}); install it or pass --firmware-code/--firmware-vars"
    )


def seed_media(state, virtio):
    seed = state / "seed"
    (seed / "drivers").mkdir(exist_ok=True)
    # The pinned ISO shares extents between identical drivers, which
    # libarchive lists as hard links into other NetKVM directories (w11/ARM64
    # links to 2k25/...). Extract the whole tree, then copy only ARM64 Win11.
    with tempfile.TemporaryDirectory(prefix="virtio-", dir=state) as scratch:
        run(["bsdtar", "-x", "-f", virtio, "-C", scratch, "NetKVM"], timeout=120)
        source = Path(scratch) / "NetKVM/w11/ARM64"
        for item in source.iterdir() if source.is_dir() else ():
            shutil.copyfile(item, seed / "drivers" / item.name)
    if not (seed / "drivers/netkvm.inf").is_file():
        raise RuntimeError("ARM64 NetKVM driver was not extracted")
    # ISO 9660 with Joliet and Rock Ridge (libarchive's defaults); Windows
    # setup reads Autounattend.xml from its root.
    run(
        [
            "bsdtar",
            "-c",
            "-f",
            state / "seed.iso",
            "--format",
            "iso9660",
            "--options",
            "iso9660:volume-id=FSWTH_SEED",
            "-C",
            seed,
            *sorted(os.listdir(seed)),
        ],
        timeout=120,
    )


def prepare(state, args):
    if not args.accept_evaluation_terms:
        raise RuntimeError(
            "Read Microsoft's evaluation terms in docs/qemu-vm.md, then pass --accept-evaluation-terms"
        )
    accel = accelerator()
    if (state / "vm.json").exists() or (state / "windows.qcow2").exists():
        raise RuntimeError(
            "State already prepared; use up. A fresh installation needs a new --state directory"
        )
    for name in TOOLS:
        tool(name)
    code, variables = firmware(args)
    media = (
        Path(args.media_dir).expanduser().resolve()
        if args.media_dir
        else state / "media"
    )
    manifest = json.loads((RESOURCES / "media.json").read_text())
    fetch(media, manifest)
    private_dir(state / "seed")
    seed = state / "seed"
    if not (state / "ssh-key").exists():
        run(
            [
                "ssh-keygen",
                "-q",
                "-t",
                "ed25519",
                "-N",
                "",
                "-C",
                "fswth-local",
                "-f",
                state / "ssh-key",
            ]
        )
    (state / "ssh-key").chmod(0o600)
    password = "Fswth!" + secrets.token_urlsafe(24)
    (state / "local-account-password").write_text(password + "\n")
    (seed / "Autounattend.xml").write_text(render_answer(password))
    for name in ("OpenSSH-ARM64.zip", "winfsp-2.1.25156.msi"):
        shutil.copyfile(media / name, seed / name)
    shutil.copyfile(state / "ssh-key.pub", seed / "authorized_keys")
    shutil.copyfile(RESOURCES / "bootstrap.ps1", seed / "bootstrap.ps1")
    seed_media(state, media / "virtio-win.iso")
    shutil.copyfile(code, state / "uefi-code.fd")
    shutil.copyfile(variables, state / "uefi-vars.fd")
    run(
        [
            "qemu-img",
            "create",
            "-f",
            "qcow2",
            state / "windows.qcow2",
            f"{args.disk_gib}G",
        ],
        timeout=30,
    )
    transport(state, args.ssh_port)
    write_json(
        state / "vm.json",
        {
            "version": 1,
            "media": str(media),
            "ssh_port": args.ssh_port,
            "cpus": args.cpus,
            "memory_mib": args.memory_mib,
            "disk_gib": args.disk_gib,
            "accelerator": accel,
            "firmware": {
                "code": str(code),
                "code_sha256": sha256(code),
                "vars": str(variables),
                "vars_sha256": sha256(variables),
            },
            "install_started": False,
            "provisioned": False,
        },
    )
    print(f"Prepared {state} for {accel}; next: up --install")


def load_config(state):
    if not (state / "vm.json").is_file():
        raise RuntimeError("VM is not prepared; run prepare first")
    config = json.loads((state / "vm.json").read_text())
    if config.get("version") != 1:
        raise RuntimeError("Unsupported VM state version")
    return config


def qemu_command(state, config, install=False):
    runtime = runtime_dir(state)
    # State prepared before HVF support has no accelerator: it was KVM.
    accel = config.get("accelerator", "kvm")
    command = [
        "qemu-system-aarch64",
        "-name",
        "fswth-local",
        "-machine",
        f"virt,accel={accel},gic-version={GIC[accel]}",
        "-cpu",
        "host",
        "-smp",
        str(config["cpus"]),
        "-m",
        str(config["memory_mib"]),
        "-drive",
        f"if=pflash,format=raw,readonly=on,file={state / 'uefi-code.fd'}",
        "-drive",
        f"if=pflash,format=raw,file={state / 'uefi-vars.fd'}",
        "-device",
        "ramfb",
        "-device",
        "qemu-xhci",
        "-device",
        "usb-kbd",
        "-device",
        "usb-tablet",
        "-display",
        "none",
        "-vnc",
        f"unix:{runtime / 'display.sock'}",
        "-qmp",
        f"unix:{runtime / 'qmp.sock'},server=on,wait=off",
        "-serial",
        f"file:{state / 'serial.log'}",
        "-monitor",
        "none",
        "-pidfile",
        str(runtime / "vm.pid"),
        "-daemonize",
        "-drive",
        f"if=none,id=system,format=qcow2,file={state / 'windows.qcow2'}",
        "-device",
        "nvme,drive=system,serial=FSWTHBOOT,bootindex=1",
        "-netdev",
        f"user,id=net0,hostfwd=tcp:127.0.0.1:{config['ssh_port']}-:22",
        "-device",
        "virtio-net-pci,netdev=net0",
    ]
    if install:
        for name, path in (
            ("installer", Path(config["media"]) / "windows11-iot-arm64-eval.iso"),
            ("seed", state / "seed.iso"),
        ):
            command += [
                "-drive",
                f"if=none,id={name},format=raw,media=cdrom,readonly=on,file={path}",
                "-device",
                f"usb-storage,drive={name}"
                + (",bootindex=2" if name == "installer" else ""),
            ]
    return command


def up(state, config, install=False):
    status = running(state)
    if status is not None:
        if not status.get("running"):
            raise RuntimeError(f"VM exists but is not running: {status}")
        print("VM is already running; boot and disk are reused")
        return
    if install and config["install_started"]:
        raise RuntimeError(
            "Installer already launched; refusing to reattach disk-wiping unattended media"
        )
    if not install and not config["install_started"]:
        raise RuntimeError("First boot needs up --install")
    command = qemu_command(state, config, install)
    # Mark before launching: a killed command must not allow a destructive retry.
    if install:
        config["install_started"] = True
        write_json(state / "vm.json", config)
    with (state / "qemu.log").open("a") as log:
        try:
            run(command, stdout=log, stderr=subprocess.STDOUT, timeout=30)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
            raise RuntimeError(
                f"QEMU launch failed; inspect {state / 'qemu.log'}"
            ) from exc
    print(
        f"VM started; SSH 127.0.0.1:{config['ssh_port']}; it stays running until down"
    )


def ssh(state, command, **kwargs):
    encoded = base64.b64encode(command.encode("utf-16-le")).decode()
    return run(
        [
            state / "bin/ssh",
            "-n",
            "fswth-local",
            "powershell.exe",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-EncodedCommand",
            encoded,
        ],
        **kwargs,
    )


def wait_ready(state, timeout):
    deadline = time.monotonic() + timeout
    command = "if ((Get-ItemProperty 'HKLM:\\SYSTEM\\Setup').SystemSetupInProgress -ne 0) { exit 1 }; 'FSWTH_READY'"
    while time.monotonic() < deadline:
        try:
            result = ssh(
                state,
                command,
                # The first shell prepares PowerShell modules: measured
                # at 23.201s on Pi. Keep it bounded without killing it at 15s.
                timeout=min(60, max(1, deadline - time.monotonic())),
                capture_output=True,
            )
            if b"FSWTH_READY" in result.stdout:
                print("Windows setup completed and SSH is ready")
                return
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
            pass
        time.sleep(min(5, max(0, deadline - time.monotonic())))
    raise TimeoutError(
        f"Windows SSH/setup not ready within {timeout}s; use screenshot and inspect guest C:\\fswth-bootstrap\\provision.log"
    )


def provision(state, config):
    wait_ready(state, 300)
    with (state / "provision.log").open("a") as log:
        run(
            [
                state / "bin/scp",
                RESOURCES / "finish.ps1",
                "fswth-local:C:/fswth-bootstrap/finish.ps1",
            ],
            stdout=log,
            stderr=subprocess.STDOUT,
            timeout=60,
        )
        ssh(
            state,
            "& C:\\fswth-bootstrap\\finish.ps1; if (-not $?) { exit 1 }",
            stdout=log,
            stderr=subprocess.STDOUT,
            timeout=600,
        )
    for block in qmp(state, "query-block"):
        if block.get("device") in ("installer", "seed") and block.get("inserted"):
            qmp(state, "eject", {"device": block["device"], "force": False})
    config["provisioned"] = True
    write_json(state / "vm.json", config)
    print(
        f"Windows activated, WinFsp installed, installer ejected; log: {state / 'provision.log'}"
    )


def down(state, timeout):
    if running(state) is None:
        print("VM is already stopped")
        return
    qmp(state, "system_powerdown")
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if running(state) is None:
            print("VM shut down cleanly")
            return
        time.sleep(2)
    raise TimeoutError("Windows did not shut down; left running, no forced kill")


def boot_installer(state, config):
    """Recover the initial UEFI Shell prompt after missing 'press any key'."""
    if not config["install_started"] or config["provisioned"]:
        raise RuntimeError(
            "boot-installer is only for the first installation's UEFI Shell"
        )
    codes = {":": "shift-semicolon", "\\": "backslash", ".": "dot", "\n": "ret"}
    for char in "fs0:\\efi\\boot\\bootaa64.efi\n":
        qmp(
            state,
            "human-monitor-command",
            {"command-line": f"sendkey {codes.get(char, char)} 20"},
        )
        time.sleep(0.05)
    for _ in range(8):
        time.sleep(1)
        qmp(state, "send-key", {"keys": [{"type": "qcode", "data": "spc"}]})
    print("Installer boot command sent; use screenshot to check progress")


def positive(value):
    number = int(value)
    if number < 1:
        raise argparse.ArgumentTypeError("must be positive")
    return number


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state", type=Path, default=DEFAULT_STATE)
    sub = parser.add_subparsers(dest="action", required=True)
    prep = sub.add_parser(
        "prepare", help="download verified media and create a fresh guest disk"
    )
    prep.add_argument("--accept-evaluation-terms", action="store_true")
    prep.add_argument(
        "--media-dir", help="reuse a local download cache (hashes always checked)"
    )
    prep.add_argument("--ssh-port", type=int, default=22260)
    prep.add_argument("--cpus", type=positive, default=2)
    prep.add_argument("--memory-mib", type=positive, default=4096)
    prep.add_argument("--disk-gib", type=positive, default=128)
    prep.add_argument("--firmware-code", help="default: discovered (docs/qemu-vm.md)")
    prep.add_argument("--firmware-vars", help="variable-store template for the code")
    start = sub.add_parser("up", help="start once; reuse a running VM")
    start.add_argument(
        "--install",
        action="store_true",
        help="first boot ONLY; unattended setup wipes this VM's disk",
    )
    for name, seconds in (("wait", 1800), ("down", 180)):
        cmd = sub.add_parser(name)
        cmd.add_argument("--timeout", type=positive, default=seconds)
    sub.add_parser("status")
    sub.add_parser(
        "boot-installer",
        help="from the initial UEFI Shell only: boot fs0's ARM64 installer",
    )
    sub.add_parser(
        "provision", help="activate evaluation and install WinFsp after Windows setup"
    )
    for name in ("exec", "ssh"):
        cmd = sub.add_parser(
            name,
            help="run a host command with VM SSH config"
            if name == "exec"
            else "run a PowerShell command",
        )
        cmd.add_argument("command", nargs=argparse.REMAINDER)
    shot = sub.add_parser("screenshot")
    shot.add_argument("output", type=Path)
    key = sub.add_parser(
        "key", help="send a QEMU key name, e.g. spc or ret, for initial installer boot"
    )
    key.add_argument("qcode")
    args = parser.parse_args(argv)
    if args.action == "prepare" and not 1024 <= args.ssh_port <= 65535:
        parser.error("--ssh-port must be in 1024..65535")
    return args


def main(argv=None):
    # macOS's /usr/bin/python3 is 3.9; fail by name, not on a missing API.
    if sys.version_info < (3, 11):
        raise RuntimeError("Python 3.11+ is required; see docs/qemu-vm.md")
    args = parse_args(argv)
    state = args.state.expanduser().resolve()
    if any(c in str(state) for c in ',"%\n\r'):
        raise RuntimeError(
            "State path cannot contain commas, double quotes, %, or newlines"
        )
    os.umask(0o077)
    if args.action == "status":
        print(json.dumps({"state": str(state), "qemu": running(state)}, indent=2))
        return 0
    with exclusive(state):
        if args.action == "prepare":
            prepare(state, args)
            return 0
        config = load_config(state)
        if args.action == "up":
            up(state, config, args.install)
        elif args.action == "wait":
            wait_ready(state, args.timeout)
        elif args.action == "provision":
            provision(state, config)
        elif args.action == "down":
            down(state, args.timeout)
        elif args.action == "boot-installer":
            boot_installer(state, config)
        elif args.action == "screenshot":
            qmp(
                state,
                "screendump",
                {"filename": str(args.output.resolve()), "format": "png"},
            )
        elif args.action == "key":
            qmp(state, "send-key", {"keys": [{"type": "qcode", "data": args.qcode}]})
        elif args.action in ("exec", "ssh"):
            command = args.command[1:] if args.command[:1] == ["--"] else args.command
            if not command:
                raise RuntimeError(f"{args.action} needs a command after --")
            if args.action == "ssh":
                ssh(state, " ".join(command), timeout=300)
            else:
                if not config["provisioned"] or running(state) is None:
                    raise RuntimeError("VM must be running and provisioned before exec")
                env = dict(
                    os.environ,
                    PATH=str(state / "bin") + os.pathsep + os.environ["PATH"],
                )
                # Keep the lock in this parent until the complete command exits.
                return subprocess.call(command, env=env)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        print(f"local-vm: {error}", file=sys.stderr)
        sys.exit(1)
