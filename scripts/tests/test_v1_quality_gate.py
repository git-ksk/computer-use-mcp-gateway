import contextlib
import importlib.util
import io
import pathlib
import sys
import unittest
from unittest import mock

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "v1_quality_gate.py"
SPEC = importlib.util.spec_from_file_location("v1_quality_gate", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class V1QualityGatePortabilityTests(unittest.TestCase):
    def test_macos_explicitly_skips_linux_proc_gate(self):
        output = io.StringIO()
        with (
            mock.patch.object(mod, "proc_ticks", side_effect=AssertionError("/proc touched")),
            mock.patch.object(mod, "proc_rss_mib", side_effect=AssertionError("/proc touched")),
            contextlib.redirect_stdout(output),
        ):
            mod.run_idle_resource_gate(999999, platform="darwin")

        text = output.getvalue()
        self.assertIn("idle resource gate SKIP", text)
        self.assertIn("platform=darwin", text)
        self.assertIn("portable health/soak checks passed", text)

    def test_windows_explicitly_skips_linux_proc_gate(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            mod.run_idle_resource_gate(999999, platform="win32")

        self.assertIn("idle resource gate SKIP", output.getvalue())
        self.assertIn("platform=win32", output.getvalue())

    def test_linux_still_enforces_cpu_threshold(self):
        with (
            mock.patch.object(mod.time, "sleep"),
            mock.patch.object(mod.time, "monotonic", side_effect=[10.0, 15.0]),
            mock.patch.object(mod.os, "sysconf", return_value=100),
            mock.patch.object(mod, "proc_ticks", side_effect=[100, 200]),
            mock.patch.object(mod, "proc_rss_mib", return_value=16.0),
        ):
            with self.assertRaisesRegex(RuntimeError, "idle gateway CPU"):
                mod.run_idle_resource_gate(1234, platform="linux")

    def test_linux_still_enforces_rss_threshold(self):
        with (
            mock.patch.object(mod.time, "sleep"),
            mock.patch.object(mod.time, "monotonic", side_effect=[10.0, 15.0]),
            mock.patch.object(mod.os, "sysconf", return_value=100),
            mock.patch.object(mod, "proc_ticks", side_effect=[100, 100]),
            mock.patch.object(mod, "proc_rss_mib", return_value=mod.MAX_IDLE_RSS_MIB + 1.0),
        ):
            with self.assertRaisesRegex(RuntimeError, "idle gateway RSS"):
                mod.run_idle_resource_gate(1234, platform="linux")


if __name__ == "__main__":
    unittest.main()
