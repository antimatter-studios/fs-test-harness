#!/usr/bin/env python3
"""Host-side lifecycle checks; real Windows evidence is recorded separately."""

import argparse
import hashlib
import importlib.util
import json
import subprocess
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "local_vm", Path(__file__).resolve().parents[1] / "scripts/local-vm.py"
)
vm = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vm)


class LocalVM(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="fswth-test-")
        self.addCleanup(self.temp.cleanup)
        self.state = Path(self.temp.name)
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
            patch.object(vm, "qmp") as qmp,
        ):
            vm.down(self.state, 10)
        qmp.assert_called_once_with(self.state, "system_powerdown")

    def test_stop_timeout_is_failure(self):
        with (
            patch.object(vm, "running", return_value={"running": True}),
            patch.object(vm, "qmp") as qmp,
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
        with (
            patch.object(vm.platform, "system", return_value="Darwin"),
            self.assertRaisesRegex(RuntimeError, "macOS support is pending"),
        ):
            vm.prepare(self.state, args)
        with (
            patch.object(vm.platform, "system", return_value="Linux"),
            patch.object(vm.platform, "machine", return_value="aarch64"),
            patch.object(vm.os, "access", return_value=False),
            self.assertRaisesRegex(RuntimeError, "/dev/kvm"),
        ):
            vm.prepare(self.state, args)

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
            if argv[0] == "7z":
                (self.state / "seed/drivers").mkdir()
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
