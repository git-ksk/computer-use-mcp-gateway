import argparse
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "v2_artifact_install.py"
SPEC = importlib.util.spec_from_file_location("v2_artifact_install", SCRIPT)
mod = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class ArtifactInstallTests(unittest.TestCase):
    CUMG = "a" * 40
    HANDOFF = "b" * 40

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cumg-artifact-install-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def executable(self, name: str) -> Path:
        path = self.root / name
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)
        return path

    def profile(self) -> Path:
        path = self.root / "profile.json"
        path.write_text(
            json.dumps({
                "schema_version": 1,
                "device_id": "mac-stable-01",
                "mcp_resource": "https://example.invalid/mcp",
                "trusted_proxy_issuer": "issuer.example",
                "trusted_proxy_subject": "operator-proxy",
                "expected_cua_version": "0.19.3",
                "cua_command": str(self.executable("cua-driver")),
                "handoff_runtime_command": str(self.executable("node")),
                "codesign_fingerprint": "A" * 40,
                "macos_team_id": "ABCDE12345",
            }),
            encoding="utf-8",
        )
        return path

    def provisioning(self) -> Path:
        root = self.root / "provisioning"
        (root / "secrets").mkdir(parents=True, mode=0o700)
        (root / "trust").mkdir(parents=True, mode=0o755)
        (root / "secrets").chmod(0o700)
        (root / "trust").chmod(0o755)
        for name in mod.SECRET_FILES:
            p = root / "secrets" / name
            p.write_text(f"secret-{name}\n", encoding="utf-8")
            p.chmod(0o600)
        for name in mod.TRUST_FILES:
            p = root / "trust" / name
            p.write_text(f"trust-{name}\n", encoding="utf-8")
            p.chmod(0o600)
        return root

    def bundle(self) -> Path:
        root = self.root / "bundle"
        (root / "bin").mkdir(parents=True)
        for name in mod.RUNTIME_BINARIES:
            p = root / "bin" / name
            p.write_text(f"#!/bin/sh\n# {name}\nexit 0\n", encoding="utf-8")
            p.chmod(0o755)
        launchd = root / "launchd"
        launchd.mkdir()
        source = Path(__file__).resolve().parents[2] / "packaging/launchd/single-mac"
        for filename in mod.PLISTS.values():
            shutil.copyfile(source / filename, launchd / filename)
        return root

    def runtime_source(self) -> Path:
        root = self.root / "runtime-source"
        (root / "handoff-root/dist").mkdir(parents=True)
        (root / "handoff-root/node_modules/werift").mkdir(parents=True)
        (root / "handoff-root/dist/index.js").write_text("export {};\n", encoding="utf-8")
        (root / "handoff-root/package.json").write_text('{"name":"handoff"}\n', encoding="utf-8")
        (root / "handoff-root/package-lock.json").write_text('{"lockfileVersion":3}\n', encoding="utf-8")
        (root / "handoff-root/node_modules/werift/package.json").write_text('{"name":"werift"}\n', encoding="utf-8")
        (root / "v2_handoff_runtime.mjs").write_text("export {};\n", encoding="utf-8")
        host = root / "takeover-webrtc-host"
        host.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        host.chmod(0o755)
        return root

    @staticmethod
    def manifest():
        return {
            "schema_version": 2,
            "package_version": "0.3.0",
            "source_commit": ArtifactInstallTests.CUMG,
            "platform": "macos",
            "architecture": "arm64",
            "hub_agent_schema_version": 4,
            "paired_handoff_commit": ArtifactInstallTests.HANDOFF,
            "install_profile": "single-mac-artifact-v1",
            "files": [],
        }

    def args(self, bundle: Path, profile: Path, provisioning: Path):
        return argparse.Namespace(
            bundle_dir=str(bundle),
            profile=str(profile),
            provisioning_dir=str(provisioning),
            install_root=str(self.root / "installed"),
            run_root=str(self.root / "run"),
            launch_agent_dir=str(self.root / "LaunchAgents"),
            preflight_only=False,
        )

    def test_profile_rejects_non_https_resource(self):
        profile = json.loads(self.profile().read_text())
        profile["mcp_resource"] = "http://example.invalid/mcp"
        path = self.root / "bad-profile.json"
        path.write_text(json.dumps(profile), encoding="utf-8")
        with self.assertRaises(mod.InstallError):
            mod.load_profile(path)

    def test_symlinked_provisioning_parent_fails_before_activation(self):
        bundle = self.bundle()
        profile = self.profile()
        provisioning = self.provisioning()
        real = provisioning / "real-secrets"
        (provisioning / "secrets").rename(real)
        (provisioning / "secrets").symlink_to(real, target_is_directory=True)
        with self.assertRaises(mod.InstallError):
            mod.verify_provisioning(provisioning)

    def test_group_writable_trust_parent_fails_before_activation(self):
        provisioning = self.provisioning()
        trust = provisioning / "trust"
        trust.chmod(0o770)
        with self.assertRaises(mod.InstallError):
            mod.verify_provisioning(provisioning)

    def test_secret_permissions_fail_before_activation(self):
        root = self.provisioning()
        (root / "secrets/hub.key").chmod(0o644)
        with self.assertRaises(mod.InstallError):
            mod.verify_provisioning(root)

    def test_clean_install_starts_paired_services_then_doctor_and_status(self):
        bundle = self.bundle()
        profile = self.profile()
        provisioning = self.provisioning()
        runtime = self.runtime_source()
        calls = []

        def runner(argv, **kwargs):
            argv = [str(x) for x in argv]
            calls.append(argv)
            if argv[:2] == ["launchctl", "print"]:
                return subprocess.CompletedProcess(argv, 1, "", "")
            if argv and argv[0].endswith("/v2_maint") and "mutation-authority-init" in argv:
                authority = Path(argv[argv.index("--authority-dir") + 1])
                self.assertFalse(authority.exists(), "installer must not pre-create authority state")
                authority.mkdir(parents=True)
                return subprocess.CompletedProcess(argv, 0, "{}\n", "")
            if argv and argv[0].endswith("/v2_doctor"):
                return subprocess.CompletedProcess(argv, 0, '{"overall":"healthy"}\n', "")
            if argv and argv[0].endswith("/v2_status"):
                return subprocess.CompletedProcess(argv, 0, '{"overall":"healthy","next_action":"none"}\n', "")
            return subprocess.CompletedProcess(argv, 0, "", "")

        with mock.patch.object(mod.sys, "platform", "darwin"), \
             mock.patch.object(mod, "current_arch", return_value="arm64"), \
             mock.patch.object(mod, "verify_bundle", return_value=self.manifest()), \
             mock.patch.object(mod.payload, "extract", return_value=runtime), \
             mock.patch.object(mod.shutil, "which", return_value="/usr/bin/fake"), \
             mock.patch.object(mod.subprocess, "run", side_effect=runner), \
             mock.patch.object(mod.time, "sleep", return_value=None):
            mod.install(self.args(bundle, profile, provisioning))

        installed = self.root / "installed"
        self.assertTrue((installed / "runtime-manifest.json").is_file())
        runtime_manifest = json.loads((installed / "runtime-manifest.json").read_text())
        self.assertEqual(runtime_manifest["source_commit"], self.CUMG)
        self.assertEqual(runtime_manifest["hub_agent_schema_version"], 4)
        self.assertEqual({x["name"] for x in runtime_manifest["binaries"]}, set(mod.RUNTIME_BINARIES))
        self.assertTrue((installed / "mutation-authority").is_dir())

        bootstraps = [x for x in calls if x[:2] == ["launchctl", "bootstrap"]]
        self.assertEqual(
            [Path(x[-1]).stem for x in bootstraps],
            list(mod.LABELS),
        )
        doctor_index = next(i for i, x in enumerate(calls) if x and x[0].endswith("/v2_doctor"))
        status_index = next(i for i, x in enumerate(calls) if x and x[0].endswith("/v2_status"))
        last_bootstrap = max(i for i, x in enumerate(calls) if x[:2] == ["launchctl", "bootstrap"])
        self.assertLess(last_bootstrap, doctor_index)
        self.assertLess(doctor_index, status_index)
        self.assertTrue(any(x and x[0].endswith("/v2_maint") and "mutation-authority-init" in x for x in calls))

    def test_existing_install_refuses_before_launchd_bootstrap(self):
        bundle = self.bundle()
        profile = self.profile()
        provisioning = self.provisioning()
        installed = self.root / "installed"
        (installed / "bin").mkdir(parents=True)
        with mock.patch.object(mod.sys, "platform", "darwin"), \
             mock.patch.object(mod, "verify_bundle", return_value=self.manifest()), \
             mock.patch.object(mod.shutil, "which", return_value="/usr/bin/fake"), \
             self.assertRaises(mod.InstallError):
            mod.install(self.args(bundle, profile, provisioning))


if __name__ == "__main__":
    unittest.main()
