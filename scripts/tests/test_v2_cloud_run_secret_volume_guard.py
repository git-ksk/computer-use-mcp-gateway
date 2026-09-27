import contextlib
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "v2_cloud_run_secret_volume_guard.py"
spec = importlib.util.spec_from_file_location("v2_cloud_run_secret_volume_guard", SCRIPT)
guard = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(guard)


def manifest(count: int, *, service_shape: bool = False):
    volumes = []
    mounts = []
    for index in range(count):
        name = f"private-volume-{index}"
        volumes.append({
            "name": name,
            "secret": {
                "secretName": f"private-secret-{index}",
                "items": [{"key": str(index + 1), "path": "value"}],
            },
        })
        mounts.append({"name": name, "mountPath": f"/private/mount-{index}"})
    runtime = {"containers": [{"volumeMounts": mounts}], "volumes": volumes}
    if service_shape:
        return {"spec": {"template": {"spec": runtime}}}
    return {"spec": runtime}


class SecretVolumeGuardTests(unittest.TestCase):
    def run_main(self, document, auth_mode):
        with tempfile.TemporaryDirectory(prefix="cumg-secret-guard-") as temp:
            path = Path(temp) / "manifest.json"
            path.write_text(json.dumps(document), encoding="utf-8")
            stdout = io.StringIO()
            previous = sys.argv
            sys.argv = [str(SCRIPT), str(path), "--auth-mode", auth_mode]
            try:
                with contextlib.redirect_stdout(stdout):
                    code = guard.main()
            finally:
                sys.argv = previous
            return code, stdout.getvalue()

    def test_oidc_service_manifest_with_six_pinned_mounts_passes(self):
        code, output = self.run_main(manifest(6, service_shape=True), "oidc_jwt")
        self.assertEqual(code, 0)
        self.assertEqual(output, "PASS secret_mounts=6 secret_volumes=6 pinned_versions=yes\n")

    def test_oauth_introspection_revision_with_seven_pinned_mounts_passes(self):
        code, output = self.run_main(manifest(7), "oauth_introspection")
        self.assertEqual(code, 0)
        self.assertIn("secret_mounts=7", output)

    def test_orphan_secret_volume_fails_without_printing_identity(self):
        document = manifest(6)
        document["spec"]["volumes"].append({
            "name": "do-not-print-orphan",
            "secret": {
                "secretName": "do-not-print-secret-name",
                "items": [{"key": "99", "path": "value"}],
            },
        })
        code, output = self.run_main(document, "oidc_jwt")
        self.assertEqual(code, 1)
        self.assertIn("FAIL code=orphan_secret_volume count=1", output)
        self.assertNotIn("do-not-print", output)

    def test_missing_volume_binding_fails(self):
        document = manifest(6)
        document["spec"]["volumes"].pop()
        code, output = self.run_main(document, "oidc_jwt")
        self.assertEqual(code, 1)
        self.assertIn("FAIL code=missing_volume_binding count=1", output)

    def test_duplicate_mount_path_and_volume_name_fail(self):
        document = manifest(6)
        document["spec"]["volumes"].append(dict(document["spec"]["volumes"][0]))
        document["spec"]["containers"][0]["volumeMounts"][1]["mountPath"] = (
            document["spec"]["containers"][0]["volumeMounts"][0]["mountPath"]
        )
        code, output = self.run_main(document, "oidc_jwt")
        self.assertEqual(code, 1)
        self.assertIn("FAIL code=duplicate_volume_name count=1", output)
        self.assertIn("FAIL code=duplicate_mount_path count=1", output)

    def test_unpinned_latest_secret_version_fails(self):
        document = manifest(6)
        document["spec"]["volumes"][0]["secret"]["items"][0]["key"] = "latest"
        code, output = self.run_main(document, "oidc_jwt")
        self.assertEqual(code, 1)
        self.assertIn("FAIL code=unpinned_secret_version count=1", output)

    def test_duplicate_secret_source_fails_even_when_volume_names_differ(self):
        document = manifest(6)
        document["spec"]["volumes"][1]["secret"] = dict(document["spec"]["volumes"][0]["secret"])
        code, output = self.run_main(document, "oidc_jwt")
        self.assertEqual(code, 1)
        self.assertIn("FAIL code=duplicate_secret_source count=1", output)

    def test_wrong_auth_profile_mount_count_fails(self):
        code, output = self.run_main(manifest(6), "oauth_introspection")
        self.assertEqual(code, 1)
        self.assertIn("FAIL code=secret_mount_count_mismatch count=1", output)


if __name__ == "__main__":
    unittest.main()
