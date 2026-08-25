import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "v2_handoff_runtime_preflight.py"
SPEC = importlib.util.spec_from_file_location("v2_handoff_runtime_preflight", SCRIPT)
mod = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


@unittest.skipUnless(shutil.which("node"), "Node is required for runtime import preflight")
class HandoffRuntimePreflightTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cumg-handoff-preflight-")
        self.root = Path(self.temp.name)
        self.node = Path(shutil.which("node")).resolve()

    def tearDown(self):
        self.temp.cleanup()

    def make_runtime(self, with_dependency=True):
        runtime = self.root / "runtime"
        dist = runtime / "dist"
        dist.mkdir(parents=True)
        (runtime / "package.json").write_text(json.dumps({"type": "module"}), encoding="utf-8")
        (dist / "index.js").write_text(
            'import { marker } from "fixturedep";\n'
            'export const ExecutionHandoffState = marker;\n',
            encoding="utf-8",
        )
        if with_dependency:
            dep = runtime / "node_modules" / "fixturedep"
            dep.mkdir(parents=True)
            (dep / "package.json").write_text(
                json.dumps({"name": "fixturedep", "type": "module", "exports": "./index.js"}),
                encoding="utf-8",
            )
            (dep / "index.js").write_text("export const marker = 1;\n", encoding="utf-8")
        return runtime, dist / "index.js"

    def test_safe_executable_symlink_resolves_to_regular_target(self):
        link = self.root / "node-link"
        link.symlink_to(self.node)
        self.assertEqual(mod.resolve_executable(link), self.node)

    def test_dangling_executable_symlink_is_refused(self):
        link = self.root / "node-link"
        link.symlink_to(self.root / "missing")
        with self.assertRaises(mod.PreflightRefusal):
            mod.resolve_executable(link)

    def test_non_executable_regular_target_is_refused(self):
        target = self.root / "not-executable"
        target.write_text("no\n", encoding="utf-8")
        os.chmod(target, 0o600)
        with self.assertRaises(mod.PreflightRefusal):
            mod.resolve_executable(target)

    def test_self_contained_staged_runtime_imports_without_source_checkout_dependencies(self):
        _, entrypoint = self.make_runtime(with_dependency=True)
        mod.verify_import(self.node, entrypoint, ["ExecutionHandoffState"])

    def test_missing_staged_runtime_dependency_is_refused(self):
        _, entrypoint = self.make_runtime(with_dependency=False)
        with self.assertRaises(mod.PreflightRefusal):
            mod.verify_import(self.node, entrypoint, ["ExecutionHandoffState"])

    def test_missing_required_export_is_refused(self):
        _, entrypoint = self.make_runtime(with_dependency=True)
        with self.assertRaises(mod.PreflightRefusal):
            mod.verify_import(self.node, entrypoint, ["TakeoverBroker"])

    def test_symlinked_staged_entrypoint_is_refused(self):
        _, entrypoint = self.make_runtime(with_dependency=True)
        link = self.root / "linked-entrypoint.js"
        link.symlink_to(entrypoint)
        with self.assertRaises(mod.PreflightRefusal):
            mod.verify_import(self.node, link, ["ExecutionHandoffState"])

    def test_invalid_required_export_name_is_refused_before_node_execution(self):
        _, entrypoint = self.make_runtime(with_dependency=True)
        with self.assertRaises(mod.PreflightRefusal):
            mod.verify_import(self.node, entrypoint, ["x;process.exit(0)"])


if __name__ == "__main__":
    unittest.main()
