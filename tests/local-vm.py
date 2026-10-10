#!/usr/bin/env python3
"""Host-side lifecycle checks; real Windows evidence is recorded separately."""

import argparse
import base64
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest.mock import MagicMock, patch

spec = importlib.util.spec_from_file_location(
    "local_vm", Path(__file__).resolve().parents[1] / "scripts/local-vm.py"
)
vm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vm)


class LocalVM(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="fswth-test-")
        self.addCleanup(self.temp.cleanup)
        # main() resolves --state; macOS temp dirs sit behind /var -> /private/var.
        self.state = Path(self.temp.name).resolve()
        self.config = {
            "version": 1,
            "cpus": 2,
            "memory_mib": 4096,
            "media": str(self.state / "media"),
            "ssh_port": 22261,
            "install_started": False,
            "provisioned": False,
        }

    def test_answer_has_no_secret_and_targets_only_disk_zero(self):
        answer = vm.render_answer("A!<&secret")
        root = ET.fromstring(answer)
        ns = {"u": "urn:schemas-microsoft-com:unattend"}
        self.assertEqual(root.find(".//u:Password/u:Value", ns).text, "A!<&secret")
        self.assertEqual([x.text for x in root.findall(".//u:DiskID", ns)], ["0", "0"])
        self.assertNotIn("@PASSWORD@", answer)
        self.assertIn("bootstrap.ps1", answer)
        self.assertNotIn("msiexec", answer)

    def test_manifest_pins_every_download(self):
        manifest = json.loads((vm.RESOURCES / "media.json").read_text())
        self.assertEqual(len(manifest), 4)
        for name, item in manifest.items():
            self.assertNotIn("/", name)
            self.assertRegex(item["sha256"], r"^[0-9a-f]{64}$")
            self.assertGreater(item["bytes"], 0)
            self.assertTrue(item["url"].startswith("https://"))
            self.assertNotIn("latest", item["url"])

    def test_size_and_hash_both_required(self):
        path = self.state / "media.iso"
        expected = {"bytes": 4, "sha256": hashlib.sha256(b"good").hexdigest()}
        self.assertFalse(vm.verify(path, expected))
        path.write_bytes(b"bad!")
        self.assertFalse(vm.verify(path, expected))
        path.write_bytes(b"good!")
        self.assertFalse(vm.verify(path, expected))
        path.write_bytes(b"good")
        self.assertTrue(vm.verify(path, expected))

    def test_bad_cache_never_downloads_over_existing_file(self):
        (self.state / "image.iso").write_bytes(b"bad")
        with (
            patch.object(vm, "run") as run,
            self.assertRaisesRegex(RuntimeError, "Cached media"),
        ):
            vm.fetch(self.state, {"image.iso": {"bytes": 5, "sha256": "a" * 64}})
        run.assert_not_called()
        self.assertEqual((self.state / "image.iso").read_bytes(), b"bad")

    def test_bad_download_is_not_promoted(self):
        def download(*args, **kwargs):
            (self.state / "image.iso.part").write_bytes(b"bad")

        with (
            patch.object(vm, "run", side_effect=download),
            patch.object(vm, "tool", return_value="curl"),
            self.assertRaisesRegex(RuntimeError, "Download hash"),
        ):
            vm.fetch(
                self.state,
                {
                    "image.iso": {
                        "bytes": 5,
                        "sha256": "a" * 64,
                        "url": "https://example.invalid/image.iso",
                    }
                },
            )
        self.assertFalse((self.state / "image.iso").exists())

    def test_qemu_uses_kvm_loopback_and_no_installer_on_normal_boot(self):
        with patch.object(vm, "runtime_dir", return_value=self.state):
            cmd = vm.qemu_command(self.state, self.config)
        text = " ".join(cmd)
        self.assertIn("virt,accel=kvm,gic-version=host", cmd)
        self.assertIn("hostfwd=tcp:127.0.0.1:22261-:22", text)
        self.assertNotIn("id=installer", text)
        self.assertNotIn("id=seed", text)
        self.assertNotIn("tcg", text)

    def test_install_attaches_both_media(self):
        with patch.object(vm, "runtime_dir", return_value=self.state):
            cmd = " ".join(vm.qemu_command(self.state, self.config, True))
        self.assertIn("usb-storage,drive=installer,bootindex=2", cmd)
        self.assertIn("usb-storage,drive=seed", cmd)
        self.assertIn("bootindex=1", cmd)

    def test_up_reuses_running_vm_without_launch_or_reboot(self):
        with (
            patch.object(vm, "running", return_value={"running": True}),
            patch.object(vm, "run") as run,
        ):
            vm.up(self.state, self.config, True)
        run.assert_not_called()

    def test_paused_vm_is_not_reported_ready(self):
        with (
            patch.object(vm, "running", return_value={"running": False}),
            self.assertRaisesRegex(RuntimeError, "not running"),
        ):
            vm.up(self.state, self.config)

    def test_up_refuses_unprepared_boot_and_second_installer(self):
        with patch.object(vm, "running", return_value=None):
            with self.assertRaisesRegex(RuntimeError, "First boot"):
                vm.up(self.state, self.config)
            self.config["install_started"] = True
            with self.assertRaisesRegex(RuntimeError, "refusing to reattach"):
                vm.up(self.state, self.config, True)

    def test_install_is_marked_before_launch_even_if_launch_fails(self):
        def launch(*args, **kwargs):
            self.assertTrue(
                json.loads((self.state / "vm.json").read_text())["install_started"]
            )
            raise subprocess.CalledProcessError(1, "qemu")

        with (
            patch.object(vm, "running", return_value=None),
            patch.object(vm, "runtime_dir", return_value=self.state),
            patch.object(vm, "run", side_effect=launch),
            self.assertRaisesRegex(RuntimeError, "QEMU launch failed"),
        ):
            vm.up(self.state, self.config, True)

    def test_exclusive_lock_refuses_another_operation(self):
        with (
            vm.exclusive(self.state),
            self.assertRaisesRegex(RuntimeError, "Another lifecycle"),
            vm.exclusive(self.state),
        ):
            self.fail("lock allowed concurrent operation")
        with vm.exclusive(self.state):
            pass

    def test_ssh_wrappers_preserve_spaces_in_paths_and_arguments(self):
        state = self.state / "state with spaces"
        state.mkdir()
        fake = self.state / "fake ssh"
        fake.write_text(
            "#!/usr/bin/env python3\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n"
        )
        fake.chmod(0o700)
        with patch.object(vm, "tool", return_value=str(fake)):
            vm.transport(state, 22261)
        result = subprocess.run(
            [state / "bin/ssh", "fswth-local", "a b"],
            check=True,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            json.loads(result.stdout),
            ["-F", str(state / "ssh_config"), "fswth-local", "a b"],
        )
        self.assertIn(
            f'IdentityFile "{state / "ssh-key"}"', (state / "ssh_config").read_text()
        )

    def test_powershell_commands_suppress_progress_before_running(self):
        # A fresh guest's first shell printed "Preparing modules for first
        # use" as CLIXML into provision.log (macOS validation, 2026-10-10).
        with patch.object(vm, "run") as run:
            vm.ssh(self.state, "& C:\\fswth-bootstrap\\finish.ps1")
        argv = run.call_args.args[0]
        script = base64.b64decode(argv[argv.index("-EncodedCommand") + 1]).decode(
            "utf-16-le"
        )
        first, rest = script.split("\n", 1)
        self.assertEqual(first, "$ProgressPreference = 'SilentlyContinue'")
        self.assertEqual(rest, "& C:\\fswth-bootstrap\\finish.ps1")

    def test_wait_requires_setup_completion_marker(self):
        responses = [
            subprocess.CompletedProcess([], 0, stdout=b"unexpected"),
            subprocess.CompletedProcess([], 0, stdout=b"FSWTH_READY\r\n"),
        ]
        with (
            patch.object(vm, "ssh", side_effect=responses) as ssh,
            patch.object(vm.time, "sleep"),
        ):
            vm.wait_ready(self.state, 20)
        self.assertEqual(ssh.call_count, 2)
        self.assertIn("SystemSetupInProgress", ssh.call_args.args[1])

    def test_wait_timeout_is_failure(self):
        with (
            patch.object(vm.time, "monotonic", side_effect=[0, 2]),
            self.assertRaises(TimeoutError),
        ):
            vm.wait_ready(self.state, 1)

    def test_failed_provision_never_marks_ready_or_ejects(self):
        with (
            patch.object(vm, "wait_ready"),
            patch.object(vm, "run"),
            patch.object(
                vm, "ssh", side_effect=subprocess.CalledProcessError(1, "ssh")
            ),
            patch.object(vm, "qmp") as qmp,
            self.assertRaises(subprocess.CalledProcessError),
        ):
            vm.provision(self.state, self.config)
        self.assertFalse(self.config["provisioned"])
        qmp.assert_not_called()

    def test_successful_provision_ejects_only_installation_media(self):
        blocks = [
            {"device": "system", "inserted": True},
            {"device": "installer", "inserted": True},
            {"device": "seed"},
        ]
        with (
            patch.object(vm, "wait_ready"),
            patch.object(vm, "run"),
            patch.object(vm, "ssh"),
            patch.object(vm, "qmp", side_effect=[blocks, {}]) as qmp,
        ):
            vm.provision(self.state, self.config)
        qmp.assert_any_call(
            self.state, "eject", {"device": "installer", "force": False}
        )
        self.assertEqual(qmp.call_count, 2)
        self.assertTrue(json.loads((self.state / "vm.json").read_text())["provisioned"])

    def test_shutdown_waits_and_never_forces_kill(self):
        with (
            patch.object(vm, "running", side_effect=[{"running": True}, None]),
            patch.object(
                vm, "ssh", side_effect=subprocess.CalledProcessError(255, "ssh")
            ),
            patch.object(vm, "qmp") as qmp,
            contextlib.redirect_stderr(io.StringIO()),
        ):
            vm.down(self.state, 10)
        qmp.assert_called_once_with(self.state, "system_powerdown")

    def test_shutdown_is_requested_inside_windows_first(self):
        # ACPI system_powerdown left Windows dirty: Kernel-Power 41 and
        # EventLog 6008 on the next boot, twice with the host awake. Windows'
        # own shutdown logged 1074/6006/109 and the next boot was clean.
        with (
            patch.object(vm, "running", side_effect=[{"running": True}, None]),
            patch.object(vm, "ssh") as ssh,
            patch.object(vm, "qmp") as qmp,
        ):
            vm.down(self.state, 10)
        self.assertIn("shutdown.exe /s /t 0", ssh.call_args.args[1])
        self.assertLessEqual(ssh.call_args.kwargs["timeout"], 60)
        qmp.assert_not_called()

    def test_shutdown_falls_back_to_acpi_only_without_ssh(self):
        for error in (
            subprocess.CalledProcessError(255, "ssh"),
            subprocess.TimeoutExpired("ssh", 60),
        ):
            with (
                self.subTest(error=type(error).__name__),
                patch.object(vm, "running", side_effect=[{"running": True}, None]),
                patch.object(vm, "ssh", side_effect=error),
                patch.object(vm, "qmp") as qmp,
                contextlib.redirect_stderr(io.StringIO()) as stderr,
            ):
                vm.down(self.state, 10)
            qmp.assert_called_once_with(self.state, "system_powerdown")
            self.assertIn("unexpected shutdown", stderr.getvalue())

    def test_stop_timeout_is_failure(self):
        with (
            patch.object(vm, "running", return_value={"running": True}),
            patch.object(
                vm, "ssh", side_effect=subprocess.CalledProcessError(255, "ssh")
            ),
            patch.object(vm, "qmp") as qmp,
            contextlib.redirect_stderr(io.StringIO()),
            patch.object(vm.time, "monotonic", side_effect=[0, 2]),
            self.assertRaises(TimeoutError),
        ):
            vm.down(self.state, 1)
        qmp.assert_called_once_with(self.state, "system_powerdown")

    def test_configuration_and_argument_validation(self):
        with self.assertRaisesRegex(RuntimeError, "prepare first"):
            vm.load_config(self.state)
        vm.write_json(self.state / "vm.json", self.config)
        self.assertEqual(vm.load_config(self.state), self.config)
        vm.write_json(self.state / "vm.json", {"version": 100})
        with self.assertRaisesRegex(RuntimeError, "Unsupported"):
            vm.load_config(self.state)
        self.assertEqual(vm.positive("2"), 2)
        with self.assertRaises(argparse.ArgumentTypeError):
            vm.positive("0")
        self.assertEqual(
            vm.parse_args(["exec", "--", "echo", "a b"]).command, ["--", "echo", "a b"]
        )

    def test_missing_tool_fails_with_prerequisite(self):
        with (
            patch.object(vm.shutil, "which", return_value=None),
            self.assertRaisesRegex(RuntimeError, "Missing qemu.*prerequisites"),
        ):
            vm.tool("qemu")

    def test_prepare_requires_terms_and_supported_host(self):
        args = vm.parse_args(["prepare"])
        with self.assertRaisesRegex(RuntimeError, "evaluation terms"):
            vm.prepare(self.state, args)
        args.accept_evaluation_terms = True
        for system, machine in (("Darwin", "x86_64"), ("Linux", "x86_64")):
            with (
                self.subTest(system=system, machine=machine),
                patch.object(vm.platform, "system", return_value=system),
                patch.object(vm.platform, "machine", return_value=machine),
                self.assertRaisesRegex(RuntimeError, "Unsupported host"),
            ):
                vm.prepare(self.state, args)
        with (
            patch.object(vm.platform, "system", return_value="Linux"),
            patch.object(vm.platform, "machine", return_value="aarch64"),
            patch.object(vm.os, "access", return_value=False),
            self.assertRaisesRegex(RuntimeError, "/dev/kvm"),
        ):
            vm.prepare(self.state, args)
        with (
            patch.object(vm.platform, "system", return_value="Darwin"),
            patch.object(vm.platform, "machine", return_value="arm64"),
            patch.object(
                vm.subprocess,
                "run",
                return_value=subprocess.CompletedProcess([], 0, stdout="0\n"),
            ),
            self.assertRaisesRegex(RuntimeError, "Hypervisor.framework"),
        ):
            vm.prepare(self.state, args)
        self.assertFalse((self.state / "vm.json").exists())

    def test_each_supported_host_selects_its_accelerator(self):
        hv_support = subprocess.CompletedProcess([], 0, stdout="1\n")
        for system, machine, expected in (
            ("Linux", "aarch64", "kvm"),
            ("Linux", "arm64", "kvm"),
            ("Darwin", "arm64", "hvf"),
        ):
            with (
                self.subTest(system=system, machine=machine),
                patch.object(vm.platform, "system", return_value=system),
                patch.object(vm.platform, "machine", return_value=machine),
                patch.object(vm.os, "access", return_value=True),
                patch.object(vm.subprocess, "run", return_value=hv_support) as run,
            ):
                self.assertEqual(vm.accelerator(), expected)
            if expected == "hvf":
                self.assertEqual(
                    run.call_args.args[0], ["sysctl", "-n", "kern.hv_support"]
                )

    def test_qemu_uses_hvf_with_emulated_gicv3(self):
        # gic-version=host is refused without KVM (QEMU 10.2.2, Apple M3 Pro).
        self.config["accelerator"] = "hvf"
        with patch.object(vm, "runtime_dir", return_value=self.state):
            cmd = vm.qemu_command(self.state, self.config)
        self.assertIn("virt,accel=hvf,gic-version=3", cmd)
        self.assertNotIn("kvm", " ".join(cmd))
        self.assertEqual(cmd[cmd.index("-cpu") + 1], "host")

    def test_firmware_override_must_name_both_files(self):
        code = self.state / "code.fd"
        code.write_bytes(b"code")
        args = vm.parse_args(["prepare", "--firmware-code", str(code)])
        with self.assertRaisesRegex(RuntimeError, "both --firmware-code"):
            vm.firmware(args)
        args.firmware_vars = str(self.state / "missing.fd")
        with self.assertRaisesRegex(RuntimeError, "missing.fd"):
            vm.firmware(args)

    def test_firmware_is_found_beside_the_qemu_installation(self):
        prefix = self.state / "Cellar/qemu/10.2.2"
        (prefix / "bin").mkdir(parents=True)
        (prefix / "share/qemu").mkdir(parents=True)
        binary = prefix / "bin/qemu-system-aarch64"
        binary.touch()
        link = self.state / "bin-qemu"
        link.symlink_to(binary)
        args = vm.parse_args(["prepare"])
        with (
            patch.object(vm, "tool", return_value=str(link)),
            patch.object(vm, "SYSTEM_FIRMWARE", (self.state / "none.fd",) * 2),
        ):
            with self.assertRaisesRegex(RuntimeError, "edk2-aarch64-code.fd"):
                vm.firmware(args)
            # QEMU's bundled aarch64 build uses the shared ARM vars template.
            for name in ("edk2-aarch64-code.fd", "edk2-arm-vars.fd"):
                (prefix / "share/qemu" / name).write_bytes(name.encode())
            self.assertEqual(
                vm.firmware(args),
                (
                    prefix / "share/qemu/edk2-aarch64-code.fd",
                    prefix / "share/qemu/edk2-arm-vars.fd",
                ),
            )
            system = (self.state / "AAVMF_CODE.fd", self.state / "AAVMF_VARS.fd")
            for path in system:
                path.write_bytes(b"debian")
            with patch.object(vm, "SYSTEM_FIRMWARE", system):
                self.assertEqual(vm.firmware(args), system)

    def test_seed_media_use_one_archiver_on_every_host(self):
        calls = []

        def run(argv, **kwargs):
            calls.append([str(x) for x in argv])
            if "-x" in argv:
                # The pinned VirtIO ISO shares extents between identical
                # drivers; libarchive lists w11/ARM64's files as hard links
                # to NetKVM/2k25/..., so the whole tree must be extracted.
                root = Path(argv[argv.index("-C") + 1]) / "NetKVM"
                for name, body in (("w11/ARM64", b"arm64"), ("2k25/amd64", b"x64")):
                    (root / name).mkdir(parents=True)
                    (root / name / "netkvm.inf").write_bytes(body)
                    (root / name / "netkvm.inf").chmod(0o555)
                    (root / name).chmod(0o555)

        (self.state / "seed").mkdir()
        (self.state / "seed/bootstrap.ps1").touch()
        with patch.object(vm, "run", side_effect=run):
            vm.seed_media(self.state, self.state / "virtio-win.iso")
        extract, create = calls
        self.assertEqual(
            extract[:4], ["bsdtar", "-x", "-f", str(self.state / "virtio-win.iso")]
        )
        self.assertEqual(extract[-1], "NetKVM")
        self.assertNotIn("--strip-components", extract)
        drivers = self.state / "seed/drivers"
        self.assertEqual(os.listdir(drivers), ["netkvm.inf"])
        self.assertEqual((drivers / "netkvm.inf").read_bytes(), b"arm64")
        self.assertEqual(
            sorted(os.listdir(self.state / "seed")), ["bootstrap.ps1", "drivers"]
        )
        self.assertEqual(
            [p.name for p in self.state.iterdir() if p.name.startswith("virtio")], []
        )
        self.assertEqual(create[0], "bsdtar")
        self.assertIn("iso9660", create)
        self.assertIn("iso9660:volume-id=FSWTH_SEED", create)
        self.assertEqual(create[create.index("-f") + 1], str(self.state / "seed.iso"))
        self.assertEqual(create[-2:], ["bootstrap.ps1", "drivers"])
        with (
            patch.object(vm, "run"),
            self.assertRaisesRegex(RuntimeError, "NetKVM driver was not extracted"),
        ):
            shutil.rmtree(drivers)
            vm.seed_media(self.state, self.state / "virtio-win.iso")

    def test_long_operations_keep_a_mac_awake_until_they_exit(self):
        # Idle sleep froze the guest mid-smoke and under both failed ACPI
        # shutdowns (pmset log, battery, sleep=1; macOS validation 2026-10-10).
        self.config["provisioned"] = True
        vm.write_json(self.state / "vm.json", self.config)
        for system, action, expected in (
            ("Darwin", ["exec", "--", "true"], True),
            ("Darwin", ["status"], False),
            ("Linux", ["exec", "--", "true"], False),
        ):
            with (
                self.subTest(system=system, action=action[0]),
                patch.object(vm.platform, "system", return_value=system),
                patch.object(vm, "running", return_value={"running": True}),
                patch.object(vm.subprocess, "call", return_value=0),
                patch.object(vm.subprocess, "Popen") as popen,
            ):
                vm.main(["--state", str(self.state), *action])
            if expected:
                popen.assert_called_once()
                self.assertEqual(
                    popen.call_args.args[0],
                    ["caffeinate", "-i", "-w", str(os.getpid())],
                )
                # Released and reaped when the command ends, not left behind.
                popen.return_value.terminate.assert_called_once()
                popen.return_value.wait.assert_called_once()
            else:
                popen.assert_not_called()

    def test_a_share_adds_shared_guest_memory_and_a_virtio_fs_device(self):
        with patch.object(vm, "runtime_dir", return_value=self.state):
            plain = vm.qemu_command(self.state, self.config)
            shared = vm.qemu_command(self.state, self.config, share=True)
        self.assertNotIn("vhost-user-fs-pci", " ".join(plain))
        self.assertNotIn("memory-backend", " ".join(plain))
        machine = shared[shared.index("-machine") + 1]
        self.assertTrue(machine.endswith(",memory-backend=mem"), machine)
        # vhost-user maps guest RAM into virtiofsd; macOS has no memfd, so
        # POSIX shared memory serves both hosts.
        self.assertIn("memory-backend-shm,id=mem,size=4096M,share=on", shared)
        self.assertIn(f"socket,id=vfs,path={self.state / 'vfs.sock'}", shared)
        self.assertIn("vhost-user-fs-pci,queue-size=1024,chardev=vfs,tag=fswth", shared)

    def test_up_with_a_share_starts_virtiofsd_before_qemu(self):
        share = self.state / "images"
        share.mkdir()
        self.config["install_started"] = True
        order = []

        def daemon(argv, **kwargs):
            order.append("virtiofsd")
            (self.state / "vfs.sock").touch()
            return MagicMock(pid=4242, poll=MagicMock(return_value=None))

        with (
            patch.object(vm, "running", return_value=None),
            patch.object(vm, "runtime_dir", return_value=self.state),
            patch.object(vm.subprocess, "Popen", side_effect=daemon) as popen,
            patch.object(
                vm, "run", side_effect=lambda *a, **k: order.append("qemu")
            ) as run,
        ):
            vm.up(self.state, self.config, share=share, virtiofsd="/opt/virtiofsd")
            self.assertEqual(vm.current_share(self.state), str(share))
        self.assertEqual(order, ["virtiofsd", "qemu"])
        self.assertEqual(
            [str(x) for x in popen.call_args.args[0]],
            [
                "/opt/virtiofsd",
                "--shared-dir",
                str(share),
                "--socket-path",
                str(self.state / "vfs.sock"),
                "--sandbox",
                "none",
                "--cache",
                "never",
            ],
        )
        self.assertIn(
            "vhost-user-fs-pci,queue-size=1024,chardev=vfs,tag=fswth",
            run.call_args.args[0],
        )

    def test_a_running_vm_is_reused_only_with_the_same_share(self):
        share = self.state / "images"
        share.mkdir()
        with (
            patch.object(vm, "running", return_value={"running": True}),
            patch.object(vm, "runtime_dir", return_value=self.state),
            patch.object(vm, "run") as run,
        ):
            with self.assertRaisesRegex(RuntimeError, "down first"):
                vm.up(self.state, self.config, share=share)
            vm.write_json(self.state / "share.json", {"dir": str(share)})
            vm.up(self.state, self.config, share=share)
            with self.assertRaisesRegex(RuntimeError, "down first"):
                vm.up(self.state, self.config)
        run.assert_not_called()

    def test_a_share_without_virtiofsd_or_a_directory_is_refused(self):
        self.config["install_started"] = True
        with (
            patch.object(vm, "running", return_value=None),
            patch.object(vm, "runtime_dir", return_value=self.state),
            patch.object(vm.shutil, "which", return_value=None),
            patch.object(vm, "VIRTIOFSD_PATHS", ()),
            patch.object(vm, "run") as run,
        ):
            with self.assertRaisesRegex(RuntimeError, "Missing virtiofsd"):
                vm.up(self.state, self.config, share=self.state)
            with self.assertRaisesRegex(RuntimeError, "not a directory"):
                vm.up(
                    self.state, self.config, share=self.state / "absent", virtiofsd="/v"
                )
        run.assert_not_called()

    def test_virtiofsd_is_stopped_when_it_or_qemu_fails_to_start(self):
        self.config["install_started"] = True
        daemon = MagicMock(pid=4242, poll=MagicMock(return_value=1))
        with (
            patch.object(vm, "running", return_value=None),
            patch.object(vm, "runtime_dir", return_value=self.state),
            patch.object(vm.subprocess, "Popen", return_value=daemon),
            patch.object(vm, "run") as run,
            self.assertRaisesRegex(RuntimeError, "virtiofsd.log"),
        ):
            vm.up(self.state, self.config, share=self.state, virtiofsd="/v")
        run.assert_not_called()
        daemon.terminate.assert_called_once()

        def listening(argv, **kwargs):
            (self.state / "vfs.sock").touch()
            return daemon

        daemon = MagicMock(pid=4242, poll=MagicMock(return_value=None))
        with (
            patch.object(vm, "running", return_value=None),
            patch.object(vm, "runtime_dir", return_value=self.state),
            patch.object(vm.subprocess, "Popen", side_effect=listening),
            patch.object(
                vm, "run", side_effect=subprocess.CalledProcessError(1, "qemu")
            ),
            self.assertRaisesRegex(RuntimeError, "QEMU launch failed"),
        ):
            vm.up(self.state, self.config, share=self.state, virtiofsd="/v")
        daemon.terminate.assert_called_once()
        self.assertIsNone(vm.current_share(self.state))

    def test_down_stops_only_its_own_virtiofsd(self):
        (self.state / "virtiofsd.pid").write_text("4242")
        vm.write_json(self.state / "share.json", {"dir": "/srv/images"})
        for name, killed in (("virtiofsd", True), ("bash", False)):
            with (
                self.subTest(name=name),
                patch.object(vm, "runtime_dir", return_value=self.state),
                patch.object(vm, "process_name", return_value=name),
                patch.object(vm.os, "kill") as kill,
            ):
                vm.stop_virtiofsd(self.state)
            if killed:
                kill.assert_called_once_with(4242, vm.signal.SIGTERM)
            else:
                kill.assert_not_called()
        self.assertIsNone(vm.current_share(self.state))
        self.assertFalse((self.state / "virtiofsd.pid").exists())

    def test_share_driver_copies_virtio_fs_from_the_pinned_iso(self):
        self.config["provisioned"] = True
        calls = []

        def run(argv, **kwargs):
            calls.append([str(x) for x in argv])
            if "-x" in argv:
                arm64 = Path(argv[argv.index("-C") + 1]) / "viofs/w11/ARM64"
                arm64.mkdir(parents=True)
                for name in ("viofs.inf", "viofs.sys", "virtiofs.exe"):
                    (arm64 / name).write_bytes(name.encode())

        with (
            patch.object(vm, "run", side_effect=run),
            patch.object(vm, "ssh") as ssh,
        ):
            vm.share_driver(self.state, self.config)
        extract, copy = calls
        self.assertEqual(extract[-1], "viofs/w11/ARM64")
        self.assertEqual(copy[-1], "fswth-local:C:/fswth-bootstrap/viofs/")
        self.assertEqual(
            sorted(Path(x).name for x in copy[1:-1]),
            ["share-driver.ps1", "viofs.inf", "viofs.sys", "virtiofs.exe"],
        )
        self.assertIn("share-driver.ps1", ssh.call_args.args[1])
        with (
            patch.object(vm, "run"),
            self.assertRaisesRegex(RuntimeError, "virtio-fs driver was not extracted"),
        ):
            vm.share_driver(self.state, self.config)

    def test_old_python_is_refused_by_name(self):
        with (
            patch.object(vm.sys, "version_info", (3, 9, 6)),
            self.assertRaisesRegex(RuntimeError, "Python 3.11"),
        ):
            vm.main(["status"])

    def test_prepare_creates_private_seed_and_persistent_configuration(self):
        args = vm.parse_args(["prepare", "--accept-evaluation-terms"])
        fw = self.state / "firmware"
        fw.write_bytes(b"firmware")
        args.firmware_code = args.firmware_vars = str(fw)

        def fetch(media, manifest):
            media.mkdir()
            for name in manifest:
                (media / name).write_bytes(b"fixture")

        def run(argv, **kwargs):
            if argv[0] == "ssh-keygen":
                (self.state / "ssh-key").write_text("test private key")
                (self.state / "ssh-key.pub").write_text("test public key")
            if argv[0] == "bsdtar" and "-x" in argv:
                (self.state / "seed/drivers/netkvm.inf").touch()

        with (
            patch.object(vm.platform, "system", return_value="Linux"),
            patch.object(vm.platform, "machine", return_value="aarch64"),
            patch.object(vm.os, "access", return_value=True),
            patch.object(vm, "tool", side_effect=lambda name: name),
            patch.object(vm, "fetch", side_effect=fetch),
            patch.object(vm, "run", side_effect=run),
        ):
            vm.prepare(self.state, args)
            with self.assertRaisesRegex(RuntimeError, "already prepared"):
                vm.prepare(self.state, args)
        config = vm.load_config(self.state)
        self.assertFalse(config["install_started"])
        self.assertFalse(config["provisioned"])
        self.assertEqual(config["disk_gib"], 128)
        self.assertEqual(config["accelerator"], "kvm")
        self.assertEqual(
            config["firmware"],
            {
                "code": str(fw),
                "code_sha256": hashlib.sha256(b"firmware").hexdigest(),
                "vars": str(fw),
                "vars_sha256": hashlib.sha256(b"firmware").hexdigest(),
            },
        )
        self.assertEqual((self.state / "ssh-key").stat().st_mode & 0o777, 0o600)
        answer = (self.state / "seed/Autounattend.xml").read_text()
        self.assertIn(
            (self.state / "local-account-password").read_text().strip(), answer
        )
        self.assertNotIn("private key", answer)

    def test_qmp_ignores_events_and_matches_response_ids(self):
        with (
            patch.object(vm.socket, "socket") as socket,
            patch.object(vm, "runtime_dir", return_value=self.state),
        ):
            stream = socket.return_value.__enter__.return_value.makefile.return_value.__enter__.return_value
            stream.readline.side_effect = [
                b'{"QMP":{}}\n',
                b'{"return":{},"id":1}\n',
                b'{"event":"RESUME"}\n',
                b'{"return":{"running":true},"id":2}\n',
            ]
            self.assertEqual(vm.qmp(self.state, "query-status"), {"running": True})
            self.assertEqual(
                json.loads(stream.write.call_args.args[0])["execute"], "query-status"
            )

    def test_qmp_errors_are_not_stopped_status(self):
        for response in (b'{"error":{"desc":"broken"},"id":1}\n', b""):
            with (
                self.subTest(response=response),
                patch.object(vm.socket, "socket") as socket,
                patch.object(vm, "runtime_dir", return_value=self.state),
            ):
                stream = socket.return_value.__enter__.return_value.makefile.return_value.__enter__.return_value
                stream.readline.side_effect = [b'{"QMP":{}}\n', response]
                with self.assertRaises(RuntimeError):
                    vm.qmp(self.state, "query-status")
        with patch.object(vm, "qmp", side_effect=ConnectionRefusedError):
            self.assertIsNone(vm.running(self.state))
        with (
            patch.object(vm, "qmp", side_effect=TimeoutError),
            self.assertRaises(TimeoutError),
        ):
            vm.running(self.state)

    def test_boot_recovery_is_only_for_initial_install(self):
        with self.assertRaisesRegex(RuntimeError, "first installation"):
            vm.boot_installer(self.state, self.config)
        self.config["install_started"] = True
        with patch.object(vm, "qmp") as qmp, patch.object(vm.time, "sleep"):
            vm.boot_installer(self.state, self.config)
        qmp.assert_any_call(
            self.state,
            "human-monitor-command",
            {"command-line": "sendkey shift-semicolon 20"},
        )
        self.assertEqual(
            sum(call.args[1] == "send-key" for call in qmp.call_args_list), 8
        )

    def test_main_exec_preserves_command_exit_status_and_transport(self):
        self.config["provisioned"] = True
        vm.write_json(self.state / "vm.json", self.config)
        with (
            patch.object(vm, "running", return_value={"running": True}),
            patch.object(vm.subprocess, "call", return_value=23) as call,
        ):
            result = vm.main(["--state", str(self.state), "exec", "--", "echo", "a b"])
        self.assertEqual(result, 23)
        self.assertEqual(call.call_args.args[0], ["echo", "a b"])
        self.assertTrue(
            call.call_args.kwargs["env"]["PATH"].startswith(str(self.state / "bin"))
        )

    def test_ready_probe_allows_measured_cold_powershell_startup(self):
        # Fresh Windows on Pi took 23.201s preparing PowerShell modules.
        # A 15s per-probe cap kept killing a healthy, initializing shell.
        with patch.object(
            vm,
            "ssh",
            return_value=subprocess.CompletedProcess([], 0, stdout=b"FSWTH_READY"),
        ) as ssh:
            vm.wait_ready(self.state, 300)
        self.assertGreaterEqual(ssh.call_args.kwargs["timeout"], 24)
        self.assertLessEqual(ssh.call_args.kwargs["timeout"], 300)

    def test_main_rejects_missing_command_and_unprovisioned_guest(self):
        vm.write_json(self.state / "vm.json", self.config)
        for command, error in (
            ([], "needs a command"),
            (["echo"], "running and provisioned"),
        ):
            with self.assertRaisesRegex(RuntimeError, error):
                vm.main(["--state", str(self.state), "exec", "--", *command])
        with self.assertRaisesRegex(RuntimeError, "State path"):
            vm.main(["--state", "/tmp/bad,dir", "status"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
