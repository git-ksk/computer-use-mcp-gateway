import argparse
import hashlib
import importlib.util
import os
from pathlib import Path
import plistlib
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "v2_handoff_runtime_cleanup.py"
spec = importlib.util.spec_from_file_location("v2_handoff_runtime_cleanup", SCRIPT)
cleanup_module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(cleanup_module)

COMMIT = "a" * 40


def write_plist(path: Path, script: Path, env_file: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(plistlib.dumps({
        "Label": "test-agent",
        "EnvironmentVariables": {
            "CUMG_V2_HANDOFF_RUNTIME_SCRIPT": str(script),
            "CUMG_V2_HANDOFF_RUNTIME_ENV_FILE": str(env_file),
        },
    }))


class RuntimeCleanupTests(unittest.TestCase):
    def fixture(self):
        temp = tempfile.TemporaryDirectory(prefix="cumg-runtime-cleanup-")
        root = Path(temp.name) / "install"
        handoff = root / "v2" / "handoff"
        rollback = root / "rollback"
        handoff.mkdir(parents=True)
        rollback.mkdir()
        env_file = handoff / "managed-runtime.env"
        env_file.write_text("", encoding="utf-8")
        os.chmod(env_file, 0o600)
        manifest = root / "runtime-manifest.json"
        manifest.write_text(
            '{"schema_version":3,"hub_agent_schema_version":5,"source_commit":"' + COMMIT + '"}',
            encoding="utf-8",
        )
        agent_plist = Path(temp.name) / "agent.plist"
        return temp, root, handoff, rollback, env_file, manifest, agent_plist

    @staticmethod
    def runtime(handoff: Path, name: str, stamp: int) -> Path:
        runtime = handoff / name
        runtime.mkdir()
        (runtime / "v2_handoff_runtime.mjs").write_text("export {};\n", encoding="utf-8")
        os.utime(runtime, ns=(stamp, stamp))
        return runtime

    def args(self, root, rollback, manifest, agent_plist, keep_recent=1):
        return argparse.Namespace(
            install_root=str(root),
            agent_plist=str(agent_plist),
            rollback_root=str(rollback),
            runtime_manifest=str(manifest),
            expected_source_commit=COMMIT,
            keep_recent=keep_recent,
            health_confirmed=True,
            apply=True,
        )

    def test_cleanup_preserves_active_legacy_rollback_and_recent_only(self):
        temp, root, handoff, rollback, env_file, manifest, agent_plist = self.fixture()
        with temp:
            active = self.runtime(handoff, "runtime-aaaaaaa-bbbbbbb", 50)
            legacy = self.runtime(handoff, "runtime-bbbbbbb-ccccccc", 40)
            recent = self.runtime(handoff, "runtime-ccccccc-ddddddd", 30)
            old = self.runtime(handoff, "runtime-ddddddd-eeeeeee", 10)
            archived_external = self.runtime(handoff, "runtime-eeeeeee-fffffff", 20)
            env_file.write_text(
                f"CUMG_V2_HANDOFF_ROOT={active / 'handoff-root'}\n"
                f"CUMG_V2_HANDOFF_WEBRTC_HOST_EXECUTABLE={active / 'takeover-webrtc-host'}\n",
                encoding="utf-8",
            )
            os.chmod(env_file, 0o600)
            write_plist(agent_plist, active / "v2_handoff_runtime.mjs", env_file)

            legacy_bundle = rollback / "runtime-upgrade-legacy"
            write_plist(
                legacy_bundle / "launchd" / "test-agent.plist",
                legacy / "v2_handoff_runtime.mjs",
                env_file,
            )
            self_contained = rollback / "runtime-upgrade-self-contained"
            write_plist(
                self_contained / "launchd" / "test-agent.plist",
                archived_external / "v2_handoff_runtime.mjs",
                env_file,
            )
            archived = self_contained / "handoff" / "runtime-generation"
            (archived / "handoff-root" / "dist").mkdir(parents=True)
            archive_files = {
                "v2_handoff_runtime.mjs": b"export {};\n",
                "handoff-root/dist/index.js": b"export {};\n",
                "handoff-root/package.json": b'{}\n',
                "handoff-root/package-lock.json": b'{}\n',
                "handoff-root/node_modules/werift/package.json": b'{}\n',
                "takeover-webrtc-host": b"host",
            }
            manifest_files = []
            for relative, payload in archive_files.items():
                target = archived / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(payload)
                manifest_files.append({"path": relative, "sha256": hashlib.sha256(payload).hexdigest()})
            import json
            (archived / "runtime-generation-manifest.json").write_text(json.dumps({
                "schema_version": 1,
                "archive_complete": True,
                "handoff_source_commit": COMMIT,
                "files": manifest_files,
            }), encoding="utf-8")

            removed, protected, retained = cleanup_module.cleanup(
                self.args(root, rollback, manifest, agent_plist)
            )
            self.assertEqual((removed, protected, retained), (2, 2, 1))
            self.assertTrue(active.exists())
            self.assertTrue(legacy.exists())
            self.assertTrue(recent.exists())
            self.assertFalse(old.exists())
            self.assertFalse(archived_external.exists())
            self.assertTrue(env_file.exists())
            self.assertTrue(manifest.exists())


    def test_archive_without_runtime_dependencies_does_not_release_rollback_reference(self):
        temp, root, handoff, rollback, env_file, manifest, agent_plist = self.fixture()
        with temp:
            active = self.runtime(handoff, "runtime-aaaaaaa-bbbbbbb", 50)
            referenced = self.runtime(handoff, "runtime-bbbbbbb-ccccccc", 10)
            env_file.write_text(f"CUMG_V2_HANDOFF_ROOT={active / 'handoff-root'}\n", encoding="utf-8")
            os.chmod(env_file, 0o600)
            write_plist(agent_plist, active / "v2_handoff_runtime.mjs", env_file)
            bundle = rollback / "runtime-upgrade-incomplete"
            write_plist(bundle / "launchd" / "test-agent.plist", referenced / "v2_handoff_runtime.mjs", env_file)
            archived = bundle / "handoff" / "runtime-generation"
            (archived / "handoff-root" / "dist").mkdir(parents=True)
            files = {
                "v2_handoff_runtime.mjs": b"export {};\n",
                "handoff-root/dist/index.js": b"export {};\n",
                "takeover-webrtc-host": b"host",
            }
            manifest_files = []
            for relative, payload in files.items():
                target = archived / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(payload)
                manifest_files.append({"path": relative, "sha256": hashlib.sha256(payload).hexdigest()})
            import json
            (archived / "runtime-generation-manifest.json").write_text(json.dumps({
                "schema_version": 1,
                "archive_complete": True,
                "handoff_source_commit": COMMIT,
                "files": manifest_files,
            }), encoding="utf-8")

            removed, protected, retained = cleanup_module.cleanup(
                self.args(root, rollback, manifest, agent_plist, keep_recent=0)
            )
            self.assertEqual((removed, protected, retained), (0, 2, 0))
            self.assertTrue(referenced.exists())

    def test_symlink_in_any_candidate_refuses_before_deleting_other_runtime(self):
        temp, root, handoff, rollback, env_file, manifest, agent_plist = self.fixture()
        with temp:
            active = self.runtime(handoff, "runtime-aaaaaaa-bbbbbbb", 50)
            old = self.runtime(handoff, "runtime-bbbbbbb-ccccccc", 10)
            unsafe = self.runtime(handoff, "runtime-ccccccc-ddddddd", 20)
            target = Path(temp.name) / "outside"
            target.write_text("do-not-touch", encoding="utf-8")
            os.symlink(target, unsafe / "link")
            env_file.write_text(
                f"CUMG_V2_HANDOFF_ROOT={active / 'handoff-root'}\n",
                encoding="utf-8",
            )
            os.chmod(env_file, 0o600)
            write_plist(agent_plist, active / "v2_handoff_runtime.mjs", env_file)

            with self.assertRaises(cleanup_module.CleanupRefusal):
                cleanup_module.cleanup(self.args(root, rollback, manifest, agent_plist, keep_recent=0))
            self.assertTrue(old.exists())
            self.assertTrue(unsafe.exists())
            self.assertEqual(target.read_text(encoding="utf-8"), "do-not-touch")

    def test_apply_requires_explicit_health_confirmation(self):
        temp, root, handoff, rollback, env_file, manifest, agent_plist = self.fixture()
        with temp:
            active = self.runtime(handoff, "runtime-aaaaaaa-bbbbbbb", 50)
            env_file.write_text(f"CUMG_V2_HANDOFF_ROOT={active / 'handoff-root'}\n", encoding="utf-8")
            os.chmod(env_file, 0o600)
            write_plist(agent_plist, active / "v2_handoff_runtime.mjs", env_file)
            args = self.args(root, rollback, manifest, agent_plist)
            args.health_confirmed = False
            with self.assertRaises(cleanup_module.CleanupRefusal):
                cleanup_module.cleanup(args)


if __name__ == "__main__":
    unittest.main()
