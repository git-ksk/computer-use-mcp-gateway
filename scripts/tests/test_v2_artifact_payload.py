import argparse
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

SCRIPT = Path(__file__).resolve().parents[1] / "v2_artifact_payload.py"
SPEC = importlib.util.spec_from_file_location("v2_artifact_payload", SCRIPT)
mod = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class ArtifactPayloadTests(unittest.TestCase):
    CUMG = "a" * 40
    HANDOFF = "b" * 40

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cumg-artifact-payload-")
        self.root = Path(self.temp.name)

    def tearDown(self):
        self.temp.cleanup()

    def make_inputs(self):
        handoff = self.root / "handoff"
        (handoff / "dist").mkdir(parents=True)
        (handoff / "dist/index.js").write_text("export {};\n", encoding="utf-8")
        (handoff / "package.json").write_text('{"name":"handoff","version":"0.3.0"}\n', encoding="utf-8")
        (handoff / "package-lock.json").write_text('{"lockfileVersion":3}\n', encoding="utf-8")
        werift = handoff / "node_modules/werift"
        werift.mkdir(parents=True)
        (werift / "package.json").write_text('{"name":"werift"}\n', encoding="utf-8")
        runtime = self.root / "v2_handoff_runtime.mjs"
        runtime.write_text("export {};\n", encoding="utf-8")
        host = self.root / "takeover-webrtc-host"
        host.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        host.chmod(0o755)
        return handoff, runtime, host

    def build(self):
        handoff, runtime, host = self.make_inputs()
        return mod.build(
            argparse.Namespace(
                handoff_source=str(handoff),
                webrtc_host=str(host),
                runtime_script=str(runtime),
                output_dir=str(self.root / "out"),
                cumg_commit=self.CUMG,
                handoff_commit=self.HANDOFF,
            )
        )

    def extract(self):
        archive = self.build()
        return mod.extract(
            argparse.Namespace(
                archive=str(archive),
                output_dir=str(self.root / "extract"),
                cumg_commit=self.CUMG,
                handoff_commit=self.HANDOFF,
            )
        )

    def test_round_trip_is_self_contained_and_commit_paired(self):
        root = self.extract()
        manifest = mod.verify_tree(root, self.CUMG, self.HANDOFF)
        self.assertEqual(manifest["cumg_source_commit"], self.CUMG)
        self.assertEqual(manifest["handoff_source_commit"], self.HANDOFF)
        paths = {record["path"] for record in manifest["files"]}
        self.assertTrue(mod.REQUIRED.issubset(paths))
        self.assertIn("handoff-root/node_modules/werift/package.json", paths)

    def test_tampered_inner_file_fails_closed(self):
        root = self.extract()
        target = root / "handoff-root/dist/index.js"
        target.write_text("tampered\n", encoding="utf-8")
        with self.assertRaises(mod.PayloadError):
            mod.verify_tree(root, self.CUMG, self.HANDOFF)

    def test_zero_length_regular_dependency_is_manifested_and_allowed(self):
        source, runtime, host = self.make_inputs()
        empty = source / "node_modules/werift/factory-function.js"
        empty.write_bytes(b"")
        archive = mod.build(argparse.Namespace(
            handoff_source=str(source), webrtc_host=str(host), runtime_script=str(runtime),
            output_dir=str(self.root / "out-empty"), cumg_commit=self.CUMG, handoff_commit=self.HANDOFF,
        ))
        extracted = mod.extract(argparse.Namespace(
            archive=str(archive), output_dir=str(self.root / "extract-empty"),
            cumg_commit=self.CUMG, handoff_commit=self.HANDOFF,
        ))
        manifest = json.loads((extracted / mod.MANIFEST).read_text(encoding="utf-8"))
        record = next(x for x in manifest["files"] if x["path"] == "handoff-root/node_modules/werift/factory-function.js")
        self.assertEqual(record["size"], 0)
        self.assertEqual(record["sha256"], hashlib.sha256(b"").hexdigest())

    def test_remaining_dependency_symlink_is_refused(self):
        handoff, runtime, host = self.make_inputs()
        target = handoff / "node_modules/werift/package.json"
        link = handoff / "node_modules/werift/unsafe-link"
        link.symlink_to(target)
        with self.assertRaises(mod.PayloadError):
            mod.build(
                argparse.Namespace(
                    handoff_source=str(handoff),
                    webrtc_host=str(host),
                    runtime_script=str(runtime),
                    output_dir=str(self.root / "bad-out"),
                    cumg_commit=self.CUMG,
                    handoff_commit=self.HANDOFF,
                )
            )

    def test_archive_path_traversal_is_refused_before_escape(self):
        archive = self.root / "bad.tar.gz"
        with archive.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as zipped:
                with tarfile.open(fileobj=zipped, mode="w") as tf:
                    payload = b"escape"
                    info = tarfile.TarInfo("handoff-runtime/../escape")
                    info.size = len(payload)
                    info.mode = stat.S_IFREG | 0o644
                    tf.addfile(info, io.BytesIO(payload))
        output = self.root / "bad-extract"
        with self.assertRaises(mod.PayloadError):
            mod.extract(
                argparse.Namespace(
                    archive=str(archive),
                    output_dir=str(output),
                    cumg_commit=self.CUMG,
                    handoff_commit=self.HANDOFF,
                )
            )
        self.assertFalse((self.root / "escape").exists())


if __name__ == "__main__":
    unittest.main()
