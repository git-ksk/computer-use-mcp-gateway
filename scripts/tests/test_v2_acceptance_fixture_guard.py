import argparse
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "v2_acceptance_fixture_guard.py"
spec = importlib.util.spec_from_file_location("v2_acceptance_fixture_guard", SCRIPT)
guard = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(guard)


class AcceptanceFixtureGuardTests(unittest.TestCase):
    def fixture(self):
        temp = tempfile.TemporaryDirectory(prefix="cumg-fixture-guard-")
        root = Path(temp.name) / "www"
        root.mkdir()
        (root / "index.html").write_text("ok\n", encoding="utf-8")
        registry = Path(temp.name) / "registry.json"
        return temp, root, registry

    def start_args(self, root, registry, bind="127.0.0.1", allow_lan=False):
        return argparse.Namespace(
            directory=str(root),
            bind=bind,
            port=0,
            log=str(root / "http.log"),
            allow_lan=allow_lan,
            registry=str(registry),
        )

    def cleanup_args(self, registry, pid=None):
        return argparse.Namespace(registry=str(registry), pid=pid, timeout=1.0)

    def check_args(self, registry):
        return argparse.Namespace(registry=str(registry))

    def test_loopback_fixture_is_detected_then_cleaned_by_exact_identity(self):
        temp, root, registry = self.fixture()
        with temp:
            self.assertEqual(guard.start_http(self.start_args(root, registry)), 0)
            data = guard._read_registry(registry)
            pid = int(data["fixtures"][0]["pid"])
            try:
                self.assertEqual(guard.check(self.check_args(registry)), 1)
                self.assertEqual(guard.cleanup(self.cleanup_args(registry, pid)), 0)
                self.assertIsNone(guard._identity(pid))
                self.assertEqual(guard.check(self.check_args(registry)), 0)
            finally:
                if guard._identity(pid) is not None and guard._matches(data["fixtures"][0]):
                    os.killpg(int(data["fixtures"][0]["pgid"]), 9)

    def test_non_loopback_requires_explicit_lan_ack(self):
        temp, root, registry = self.fixture()
        with temp:
            with self.assertRaises(RuntimeError):
                guard.start_http(self.start_args(root, registry, bind="0.0.0.0", allow_lan=False))

    def test_explicit_lan_fixture_is_cleaned_by_exact_identity(self):
        temp, root, registry = self.fixture()
        with temp:
            self.assertEqual(
                guard.start_http(self.start_args(root, registry, bind="0.0.0.0", allow_lan=True)),
                0,
            )
            data = guard._read_registry(registry)
            entry = data["fixtures"][0]
            pid = int(entry["pid"])
            try:
                self.assertTrue(entry["lan_exposure"])
                self.assertEqual(guard.cleanup(self.cleanup_args(registry, pid)), 0)
                self.assertIsNone(guard._identity(pid))
            finally:
                if guard._identity(pid) is not None and guard._matches(entry):
                    os.killpg(int(entry["pgid"]), 9)

    def test_cleanup_refuses_reused_or_unrelated_pid_identity(self):
        temp, _, registry = self.fixture()
        with temp:
            proc = subprocess.Popen(["sleep", "30"], start_new_session=True)
            try:
                identity = guard._identity(proc.pid)
                self.assertIsNotNone(identity)
                entry = {
                    **identity,
                    "kind": "python_http_server",
                    "bind": "127.0.0.1",
                    "port": 12345,
                    "lan_exposure": False,
                    "command_sha256": "0" * 64,
                }
                guard._write_registry(registry, {"schema_version": 1, "fixtures": [entry]})
                self.assertEqual(guard.cleanup(self.cleanup_args(registry)), 2)
                self.assertIsNone(proc.poll())
            finally:
                proc.terminate()
                proc.wait(timeout=5)

    def test_dead_fixture_record_is_pruned_on_check(self):
        temp, _, registry = self.fixture()
        with temp:
            guard._write_registry(
                registry,
                {
                    "schema_version": 1,
                    "fixtures": [{
                        "pid": 999999,
                        "pgid": 999999,
                        "start": "never",
                        "command_sha256": "0" * 64,
                        "kind": "python_http_server",
                        "bind": "127.0.0.1",
                        "port": 12345,
                        "lan_exposure": False,
                    }],
                },
            )
            self.assertEqual(guard.check(self.check_args(registry)), 0)
            self.assertEqual(guard._read_registry(registry)["fixtures"], [])


if __name__ == "__main__":
    unittest.main()
