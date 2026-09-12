import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "v2_windows_upgrade.py"
SPEC = importlib.util.spec_from_file_location("v2_windows_upgrade", SCRIPT)
mod = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class FakeController:
    def __init__(self, fail_hub_once=False, fail_agent_once=False):
        self.events = []
        self.fail_hub_once = fail_hub_once
        self.fail_agent_once = fail_agent_once
        self.external_smoke_configured = False

    def validate(self):
        self.events.append("validate")

    def stop_pair(self, hub, agent):
        self.events.append("stop_pair")

    def start_hub(self):
        self.events.append("start_hub")

    def start_agent(self):
        self.events.append("start_agent")

    def wait_hub_healthy(self, hub, timeout_seconds):
        self.events.append("hub_health")
        if self.fail_hub_once:
            self.fail_hub_once = False
            raise mod.UpgradeError("forced_hub_activation_failure")

    def wait_agent_stable(self, agent, stable_seconds, timeout_seconds):
        self.events.append("agent_health")
        if self.fail_agent_once:
            self.fail_agent_once = False
            raise mod.UpgradeError("forced_agent_activation_failure")

    def external_smoke(self):
        self.events.append("external_smoke")


class WindowsUpgradeTests(unittest.TestCase):
    COMMIT = "a" * 40

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cumg-windows-upgrade-")
        self.root = Path(self.temp.name)
        self.data_root = self.root / "data"
        self.bin_dir = self.data_root / "bin"
        self.logs = self.data_root / "logs"
        self.run_dir = self.data_root / "v2-windows-shell" / "run"
        self.state_dir = self.data_root / "v2-windows-shell" / "state"
        self.trust_dir = self.data_root / "v2-windows-shell" / "trust"
        for path in (self.bin_dir, self.logs, self.run_dir, self.state_dir, self.trust_dir):
            path.mkdir(parents=True, exist_ok=True)
        (self.state_dir / "durable-sentinel").write_text("STATE-MUST-STAY", encoding="utf-8")
        (self.trust_dir / "trust-sentinel").write_text("TRUST-MUST-STAY", encoding="utf-8")

        self.active_hub = self.run_dir / "hub.json"
        self.active_agent = self.run_dir / "agent.json"
        self.candidate_dir = self.root / "candidate-config"
        self.candidate_dir.mkdir()
        self.candidate_hub = self.candidate_dir / "hub.json"
        self.candidate_agent = self.candidate_dir / "agent.json"
        self._write_config(self.active_hub, "hub", new=False)
        self._write_config(self.active_agent, "agent", new=False)
        self._write_config(self.candidate_hub, "hub", new=True)
        self._write_config(self.candidate_agent, "agent", new=True)

        self.bundle = self.root / "bundle"
        (self.bundle / "bin").mkdir(parents=True)
        files = {}
        for name in mod.WINDOWS_BINARIES:
            payload = f"new-{name}\n".encode()
            (self.bundle / "bin" / name).write_bytes(payload)
            files[name] = hashlib.sha256(payload).hexdigest()
        self.identity = mod.CandidateIdentity("0.4.0", self.COMMIT, 6, files)

        # The old fixture deliberately has no tls_check to prove rollback removes
        # files that did not exist before the attempted upgrade.
        for name in mod.WINDOWS_BINARIES:
            if name == "v2_tls_check.exe":
                continue
            (self.bin_dir / name).write_bytes(f"old-{name}\n".encode())
        self.old_binary_bytes = {
            name: (self.bin_dir / name).read_bytes()
            for name in mod.WINDOWS_BINARIES
            if (self.bin_dir / name).exists()
        }
        self.old_hub_config = self.active_hub.read_bytes()
        self.old_agent_config = self.active_agent.read_bytes()

    def tearDown(self):
        self.temp.cleanup()

    def _write_config(self, path: Path, component: str, *, new: bool, include_file_root=True):
        args = []
        if component == "hub":
            args = [
                "--bind", "127.0.0.1:7443",
                "--hub-secret-file", str(self.data_root / "secret" / "hub.key"),
                "--grant-secret-file", str(self.data_root / "secret" / "grant.key"),
                "--device-public-key-file", str(self.data_root / "trust" / "device.pub"),
                "--tls-cert-pem-file", str(self.data_root / "tls" / "tls.pem"),
                "--tls-key-pem-file", str(self.data_root / "tls" / "tls.key"),
                "--state-dir", str(self.state_dir / "hub"),
                "--mcp-bind", "127.0.0.1:8102",
                "--mcp-resource", "https://example.invalid/mcp",
                "--northbound-policy-file", str(self.trust_dir / "northbound.json"),
                "--trusted-proxy-issuer", "https://example.invalid",
                "--trusted-proxy-subject", "fixture",
                "--trusted-proxy-secret-file", str(self.data_root / "secret" / "proxy"),
            ]
        else:
            args = [
                "--hub-endpoint", "https://127.0.0.1:7443",
                "--hub-domain", "localhost",
                "--device-id", "dev_fixture",
                "--device-secret-file", str(self.data_root / "secret" / "device.key"),
                "--hub-public-key-file", str(self.trust_dir / "hub.pub"),
                "--grant-public-key-file", str(self.trust_dir / "grant.pub"),
                "--tls-root-der-file", str(self.trust_dir / "tls.der"),
                "--state-dir", str(self.state_dir / "agent"),
                "--allowed-cwd-root", str(self.root),
            ]
            if new and include_file_root:
                args += ["--allowed-file-root", str(self.root)]
        value = {
            "component": component,
            "executable": str(self.bin_dir / f"v2_{component}.exe"),
            "workingDirectory": str(self.data_root),
            "logDirectory": str(self.logs),
            "restartDelaySeconds": 2,
            "arguments": args,
        }
        path.write_text(json.dumps(value), encoding="utf-8")

    def reviewed_configs(self):
        return (
            mod.load_reviewed_config(self.candidate_hub, "hub", self.data_root),
            mod.load_reviewed_config(self.candidate_agent, "agent", self.data_root),
        )

    def perform(self, controller):
        hub, agent = self.reviewed_configs()
        return mod.perform_upgrade(
            bundle_dir=self.bundle,
            identity=self.identity,
            data_root=self.data_root,
            active_hub_config=self.active_hub,
            active_agent_config=self.active_agent,
            candidate_hub_config=hub,
            candidate_agent_config=agent,
            controller=controller,
            stable_seconds=3,
            timeout_seconds=10,
            preflight_only=False,
        )

    def assert_previous_restored(self):
        for name, payload in self.old_binary_bytes.items():
            self.assertEqual((self.bin_dir / name).read_bytes(), payload)
        self.assertFalse((self.bin_dir / "v2_tls_check.exe").exists())
        self.assertEqual(self.active_hub.read_bytes(), self.old_hub_config)
        self.assertEqual(self.active_agent.read_bytes(), self.old_agent_config)
        self.assertEqual((self.state_dir / "durable-sentinel").read_text(), "STATE-MUST-STAY")
        self.assertEqual((self.trust_dir / "trust-sentinel").read_text(), "TRUST-MUST-STAY")

    def test_v03_style_agent_config_missing_allowed_file_root_is_rejected_preflight(self):
        old_candidate = self.candidate_dir / "agent-old.json"
        self._write_config(old_candidate, "agent", new=True, include_file_root=False)
        config = mod.load_reviewed_config(old_candidate, "agent", self.data_root)
        with self.assertRaisesRegex(mod.UpgradeError, "candidate_agent_missing_allowed_file_root"):
            mod.validate_required_flags(config, {"--hub-endpoint", "--allowed-cwd-root", "--allowed-file-root"})

    def test_successful_upgrade_replaces_complete_reviewed_windows_set(self):
        controller = FakeController()
        record = self.perform(controller)
        self.assertEqual(record["status"], "completed")
        self.assertEqual(controller.events, ["validate", "stop_pair", "start_hub", "hub_health", "start_agent", "agent_health"])
        for name, expected in self.identity.files.items():
            self.assertEqual(mod.sha256_file(self.bin_dir / name), expected)
        self.assertEqual(self.active_hub.read_bytes(), self.candidate_hub.read_bytes())
        self.assertEqual(self.active_agent.read_bytes(), self.candidate_agent.read_bytes())
        self.assertEqual((self.state_dir / "durable-sentinel").read_text(), "STATE-MUST-STAY")
        self.assertEqual((self.trust_dir / "trust-sentinel").read_text(), "TRUST-MUST-STAY")
        status = json.loads(mod.status_path(self.data_root).read_text())
        self.assertEqual(status["candidate"]["source_commit"], self.COMMIT)
        self.assertEqual(status["result"], "upgraded")

    def test_forced_hub_health_failure_rolls_back_binaries_and_config(self):
        controller = FakeController(fail_hub_once=True)
        with self.assertRaisesRegex(mod.UpgradeError, "forced_hub_activation_failure"):
            self.perform(controller)
        self.assert_previous_restored()
        status = json.loads(mod.status_path(self.data_root).read_text())
        self.assertEqual(status["status"], "rolled_back")
        self.assertEqual(status["result"], "forced_hub_activation_failure")

    def test_forced_agent_health_failure_rolls_back_binaries_and_config(self):
        controller = FakeController(fail_agent_once=True)
        with self.assertRaisesRegex(mod.UpgradeError, "forced_agent_activation_failure"):
            self.perform(controller)
        self.assert_previous_restored()
        status = json.loads(mod.status_path(self.data_root).read_text())
        self.assertEqual(status["status"], "rolled_back")
        self.assertGreaterEqual(controller.events.count("start_hub"), 2)
        self.assertGreaterEqual(controller.events.count("start_agent"), 2)

    def test_partial_binary_copy_failure_restores_pair_before_any_candidate_start(self):
        controller = FakeController()
        original = mod.atomic_copy
        failed = False

        def fail_agent_activation(source, destination):
            nonlocal failed
            if not failed and destination == self.bin_dir / "v2_agent.exe" and "staging" in source.parts:
                failed = True
                raise mod.UpgradeError("forced_partial_activation_failure")
            return original(source, destination)

        with mock.patch.object(mod, "atomic_copy", side_effect=fail_agent_activation):
            with self.assertRaisesRegex(mod.UpgradeError, "forced_partial_activation_failure"):
                self.perform(controller)
        self.assert_previous_restored()
        # No candidate Hub was started: the next starts are rollback starts only.
        self.assertEqual(controller.events[:2], ["validate", "stop_pair"])
        self.assertEqual(controller.events[2], "stop_pair")

    def test_preflight_only_never_stops_runtime(self):
        controller = FakeController()
        hub, agent = self.reviewed_configs()
        record = mod.perform_upgrade(
            bundle_dir=self.bundle,
            identity=self.identity,
            data_root=self.data_root,
            active_hub_config=self.active_hub,
            active_agent_config=self.active_agent,
            candidate_hub_config=hub,
            candidate_agent_config=agent,
            controller=controller,
            stable_seconds=3,
            timeout_seconds=10,
            preflight_only=True,
        )
        self.assertEqual(record["status"], "preflight_passed")
        self.assertEqual(controller.events, ["validate"])
        self.assert_previous_restored()


if __name__ == "__main__":
    unittest.main()
