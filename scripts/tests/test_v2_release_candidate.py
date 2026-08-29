import argparse
import copy
import gzip
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "v2_release_candidate.py"
SPEC = importlib.util.spec_from_file_location("v2_release_candidate", SCRIPT)
mod = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class ReleaseCandidateTests(unittest.TestCase):
    COMMIT = "a" * 40
    VERSION = "0.3.0"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cumg-release-candidate-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def make_binary_dir(self, platform_name: str) -> Path:
        binary_dir = self.root / f"bin-{platform_name}"
        binary_dir.mkdir()
        suffix = mod.executable_suffix(platform_name)
        for name in mod.PLATFORM_BINARIES[platform_name]:
            path = binary_dir / f"{name}{suffix}"
            path.write_bytes(f"fixture-{platform_name}-{name}\n".encode())
            path.chmod(0o755)
        return binary_dir

    def build(self, platform_name: str = "macos") -> tuple[Path, Path]:
        binary_dir = self.make_binary_dir(platform_name)
        output = self.root / f"dist-{platform_name}"
        archive = mod.build_candidate(
            argparse.Namespace(
                binary_dir=str(binary_dir),
                output_dir=str(output),
                package_version=self.VERSION,
                source_commit=self.COMMIT,
                platform=platform_name,
                architecture="test-arch",
            )
        )
        return archive, Path(f"{archive}.sha256")

    def extract(self, platform_name: str = "macos") -> Path:
        archive, checksum = self.build(platform_name)
        return mod.verify_archive(archive, checksum, self.root / f"extract-{platform_name}")

    @staticmethod
    def load_manifest(bundle: Path) -> dict:
        return json.loads((bundle / mod.MANIFEST_NAME).read_text(encoding="utf-8"))

    @staticmethod
    def store_manifest(bundle: Path, manifest: dict) -> None:
        (bundle / mod.MANIFEST_NAME).write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def test_macos_candidate_round_trip_has_exact_allowlist(self):
        bundle = self.extract("macos")
        manifest = mod.verify_bundle_dir(bundle)
        self.assertEqual(manifest["source_commit"], self.COMMIT)
        self.assertEqual(
            {record["path"] for record in manifest["files"]},
            set(mod.expected_binary_paths("macos")),
        )
        self.assertIn("bin/v2_doctor", mod.expected_binary_paths("macos"))

    def test_windows_zip_candidate_round_trip_omits_unix_only_binaries(self):
        bundle = self.extract("windows")
        mod.verify_bundle_dir(bundle)
        paths = mod.expected_binary_paths("windows")
        self.assertNotIn("bin/v2_doctor.exe", paths)
        self.assertNotIn("bin/v2_grant_signer.exe", paths)
        self.assertNotIn("bin/v2_handoff_ctl.exe", paths)
        self.assertTrue((bundle / "bin/v2_hub.exe").is_file())

    def test_smoke_resolves_relative_bundle_before_spawning(self):
        bundle = self.extract("macos")
        relative = bundle.relative_to(Path.cwd()) if bundle.is_relative_to(Path.cwd()) else bundle
        completed = mock.Mock(returncode=0)
        with mock.patch.object(mod, "current_platform", return_value="macos"), mock.patch.object(
            mod.host_platform, "platform", return_value="test-host"
        ), mock.patch.object(mod.subprocess, "run", return_value=completed) as run:
            mod.smoke_bundle(relative)
        for call in run.call_args_list:
            binary = Path(call.args[0][0])
            self.assertTrue(binary.is_absolute())

    def test_invalid_source_commit_is_refused_before_artifact_creation(self):
        binary_dir = self.make_binary_dir("linux")
        with self.assertRaises(mod.CandidateError):
            mod.build_candidate(
                argparse.Namespace(
                    binary_dir=str(binary_dir),
                    output_dir=str(self.root / "dist-invalid"),
                    package_version=self.VERSION,
                    source_commit="short",
                    platform="linux",
                    architecture="x64",
                )
            )

    def test_symlinked_source_binary_is_refused(self):
        binary_dir = self.make_binary_dir("linux")
        victim = binary_dir / "v2_hub"
        target = binary_dir / "fixture-target"
        target.write_text("target\n", encoding="utf-8")
        target.chmod(0o755)
        victim.unlink()
        victim.symlink_to(target)
        with self.assertRaises(mod.CandidateError):
            mod.build_candidate(
                argparse.Namespace(
                    binary_dir=str(binary_dir),
                    output_dir=str(self.root / "dist-symlink"),
                    package_version=self.VERSION,
                    source_commit=self.COMMIT,
                    platform="linux",
                    architecture="x64",
                )
            )

    def test_extra_bundle_file_is_refused(self):
        bundle = self.extract("linux")
        (bundle / "secret.env").write_text("must-not-ship\n", encoding="utf-8")
        with self.assertRaises(mod.CandidateError):
            mod.verify_bundle_dir(bundle)

    def test_missing_manifested_file_is_refused(self):
        bundle = self.extract("linux")
        (bundle / "bin/v2_hub").unlink()
        with self.assertRaises(mod.CandidateError):
            mod.verify_bundle_dir(bundle)

    def test_manifest_hash_mismatch_is_refused(self):
        bundle = self.extract("linux")
        path = bundle / "bin/v2_hub"
        path.write_bytes(path.read_bytes() + b"tamper")
        with self.assertRaises(mod.CandidateError):
            mod.verify_bundle_dir(bundle)

    def test_manifest_size_mismatch_is_refused(self):
        bundle = self.extract("linux")
        manifest = self.load_manifest(bundle)
        manifest["files"][0]["size"] += 1
        self.store_manifest(bundle, manifest)
        with self.assertRaises(mod.CandidateError):
            mod.verify_bundle_dir(bundle)

    def test_unsafe_manifest_path_is_refused(self):
        bundle = self.extract("linux")
        manifest = self.load_manifest(bundle)
        manifest["files"][0]["path"] = "../escape"
        self.store_manifest(bundle, manifest)
        with self.assertRaises(mod.CandidateError):
            mod.verify_bundle_dir(bundle)

    def test_duplicate_manifest_path_is_refused(self):
        bundle = self.extract("linux")
        manifest = self.load_manifest(bundle)
        manifest["files"][1] = copy.deepcopy(manifest["files"][0])
        self.store_manifest(bundle, manifest)
        with self.assertRaises(mod.CandidateError):
            mod.verify_bundle_dir(bundle)

    def test_archive_checksum_mismatch_is_refused(self):
        archive, checksum = self.build("linux")
        with archive.open("ab") as handle:
            handle.write(b"tamper")
        with self.assertRaises(mod.CandidateError):
            mod.verify_archive(archive, checksum, self.root / "checksum-extract")

    def test_archive_path_traversal_is_refused_before_extraction(self):
        archive = self.root / "bad.tar.gz"
        with archive.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as zipped:
                with tarfile.open(fileobj=zipped, mode="w") as tf:
                    payload = b"escape"
                    info = tarfile.TarInfo("bad/../escape")
                    info.size = len(payload)
                    info.mode = stat.S_IFREG | 0o644
                    tf.addfile(info, io.BytesIO(payload))
        checksum = Path(f"{archive}.sha256")
        checksum.write_text(
            f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n",
            encoding="ascii",
        )
        extract = self.root / "bad-extract"
        with self.assertRaises(mod.CandidateError):
            mod.verify_archive(archive, checksum, extract)
        self.assertFalse(extract.exists())
        self.assertFalse((self.root / "escape").exists())


if __name__ == "__main__":
    unittest.main()
