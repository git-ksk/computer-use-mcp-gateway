import importlib.util
import pathlib
import os
import plistlib
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "v2_launchd_maintenance_job.py"
SPEC = importlib.util.spec_from_file_location("v2_launchd_maintenance_job", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)

DOMAIN = "gui/501"
LEGACY = "com.git-ksk.cumg-v2-upgrade-once.1787651265"


class FakeLaunchctl:
    def __init__(self, *, job_exit=0, loaded=None, running=None):
        self.job_exit = job_exit
        self.loaded = dict(loaded or {})
        self.running = set(running or ())
        self.calls = []
        self.plists = []

    def _domain_output(self):
        return "\n".join(f"0\t0\t{label}" for label in sorted(self.loaded)) + "\n"

    def _job_output(self, label):
        record = self.loaded[label]
        state = "running" if label in self.running else "not running"
        pid = "\tpid = 4242\n" if label in self.running else ""
        last_exit = "(never exited)" if label in self.running else str(record["exit"])
        return (
            f"state = {state}\n"
            f"\truns = {record['runs']}\n"
            f"{pid}"
            f"\tlast exit code = {last_exit}\n"
            "\tproperties = runatload\n"
        )

    def __call__(self, argv, *, stdout, stderr, text, check):
        self.calls.append(tuple(argv))
        action = argv[1]
        if action == "print" and argv[2] == DOMAIN:
            return subprocess.CompletedProcess(argv, 0, self._domain_output(), "")
        if action == "print":
            label = argv[2].split("/", 2)[-1]
            if label not in self.loaded:
                return subprocess.CompletedProcess(argv, 113, "", "not found")
            return subprocess.CompletedProcess(argv, 0, self._job_output(label), "")
        if action == "bootstrap":
            plist_path = pathlib.Path(argv[3])
            payload = plistlib.loads(plist_path.read_bytes())
            self.plists.append(payload)
            label = payload["Label"]
            # Simulate launchd RunAtLoad execution completing once, including non-zero exit.
            self.loaded[label] = {"runs": 1, "exit": self.job_exit}
            self.running.discard(label)
            return subprocess.CompletedProcess(argv, 0, "", "")
        if action == "bootout":
            label = argv[2].split("/", 2)[-1]
            self.loaded.pop(label, None)
            self.running.discard(label)
            return subprocess.CompletedProcess(argv, 0, "", "")
        raise AssertionError(f"unexpected launchctl call: {argv}")


class LaunchdMaintenanceTests(unittest.TestCase):
    def test_legacy_and_current_labels_are_detected_without_other_service_names(self):
        current = "com.github.git-ksk.cumg-v2-maintenance.upgrade.1.2.deadbeef"
        output = f"0 0 {LEGACY}\n0 0 {current}\n0 0 com.github.git-ksk.cumg-v2-hub\n"
        self.assertEqual(mod.labels_from_domain_output(output), tuple(sorted((current, LEGACY))))

    def test_status_parser_reports_only_bounded_launchd_state(self):
        status = mod.parse_job_status(
            LEGACY,
            "path = /private/secret\nstate = running\nruns = 7\npid = 42\nlast exit code = 2\n",
        )
        self.assertTrue(status.running)
        self.assertEqual(status.runs, 7)
        self.assertEqual(status.last_exit_code, 2)
        self.assertFalse(hasattr(status, "path"))

    def test_running_job_accepts_launchd_never_exited_sentinel_only(self):
        status = mod.parse_job_status(
            LEGACY,
            "state = running\nruns = 1\npid = 42\nlast exit code = (never exited)\n",
        )
        self.assertTrue(status.running)
        self.assertIsNone(status.last_exit_code)
        with self.assertRaisesRegex(mod.MaintenanceError, "invalid_maintenance_job_status"):
            mod.parse_job_status(
                LEGACY,
                "state = running\nruns = 1\npid = 42\nlast exit code = unknown\n",
            )

    def test_assert_clear_rejects_stale_job_and_can_exclude_current_one_shot(self):
        fake = FakeLaunchctl(loaded={LEGACY: {"runs": 1, "exit": 0}})
        with self.assertRaisesRegex(mod.MaintenanceError, "stale_maintenance_jobs"):
            mod.assert_no_stale_jobs(domain=DOMAIN, launchctl="/bin/launchctl", runner=fake)
        mod.assert_no_stale_jobs(
            domain=DOMAIN,
            launchctl="/bin/launchctl",
            exclude_label=LEGACY,
            runner=fake,
        )

    def test_cleanup_refuses_active_job_but_boots_out_completed_stale_job(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp) / "jobs"
            fake_active = FakeLaunchctl(
                loaded={LEGACY: {"runs": 1, "exit": 0}}, running={LEGACY}
            )
            with self.assertRaisesRegex(mod.MaintenanceError, "active_maintenance_job"):
                mod.cleanup_stale_jobs(
                    domain=DOMAIN,
                    launchctl="/bin/launchctl",
                    job_dir=root,
                    runner=fake_active,
                )
            self.assertIn(LEGACY, fake_active.loaded)

            fake_done = FakeLaunchctl(loaded={LEGACY: {"runs": 1, "exit": 0}})
            cleaned = mod.cleanup_stale_jobs(
                domain=DOMAIN,
                launchctl="/bin/launchctl",
                job_dir=root,
                runner=fake_done,
            )
            self.assertEqual(cleaned, 1)
            self.assertEqual(fake_done.loaded, {})

    def test_job_dir_rejects_symlink_component_and_weak_existing_permissions(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            real = root / "real"
            real.mkdir(mode=0o700)
            link = root / "link"
            link.symlink_to(real, target_is_directory=True)
            with self.assertRaisesRegex(mod.MaintenanceError, "unsafe_maintenance_job_dir"):
                mod._secure_job_dir(link / "jobs")

            weak = root / "weak"
            weak.mkdir(mode=0o777)
            weak.chmod(0o777)
            with self.assertRaisesRegex(mod.MaintenanceError, "unsafe_maintenance_job_dir"):
                mod._secure_job_dir(weak)

    def test_private_plist_is_owner_private_regular_file(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            path = root / "job.plist"
            mod._write_private_plist(path, b"payload")
            metadata = path.lstat()
            self.assertTrue(path.is_file())
            self.assertEqual(metadata.st_uid, os.getuid())
            self.assertEqual(metadata.st_nlink, 1)
            self.assertEqual(metadata.st_mode & 0o077, 0)

    def test_upgrade_environment_forwards_closed_non_secret_allowlist_only(self):
        label = "com.github.git-ksk.cumg-v2-maintenance.upgrade.1.2.deadbeef"
        environment = mod.safe_upgrade_environment(
            {
                "PATH": "/usr/bin",
                "HOME": "/Users/test",
                "CUMG_V2_EXPECTED_CUA_VERSION": "0.19.3",
                "CUMG_V2_MACOS_TEAM_ID": "ABCDEFGHIJ",
                "AWS_SECRET_ACCESS_KEY": "must-not-cross",
                "GITHUB_TOKEN": "must-not-cross",
            },
            job_label=label,
        )
        self.assertEqual(environment["CUMG_V2_MAINTENANCE_JOB_LABEL"], label)
        self.assertEqual(environment["CUMG_V2_EXPECTED_CUA_VERSION"], "0.19.3")
        self.assertNotIn("AWS_SECRET_ACCESS_KEY", environment)
        self.assertNotIn("GITHUB_TOKEN", environment)

    def test_xpcproxy_transition_is_not_mistaken_for_completed_job(self):
        label = "com.github.git-ksk.cumg-v2-maintenance.upgrade.1.2.deadbeef"
        outputs = iter([
            "state = xpcproxy\nruns = 1\npid = 42\nlast exit code = (never exited)\n",
            "state = running\nruns = 1\npid = 43\nlast exit code = (never exited)\n",
            "state = not running\nruns = 1\nlast exit code = 0\n",
            "state = not running\nruns = 1\nlast exit code = 0\n",
        ])

        def runner(argv, *, stdout, stderr, text, check):
            self.assertEqual(argv[1], "print")
            return subprocess.CompletedProcess(argv, 0, next(outputs), "")

        exit_code = mod.wait_for_one_shot_completion(
            domain=DOMAIN,
            label=label,
            launchctl="/bin/launchctl",
            timeout_secs=5,
            runner=runner,
            sleep=lambda _seconds: None,
        )
        self.assertEqual(exit_code, 0)

    def test_nonzero_upgrade_exit_runs_once_and_cleanup_always_boots_out_and_unlinks_plist(self):
        with tempfile.TemporaryDirectory() as temp:
            root = pathlib.Path(temp)
            repo = root / "repo"
            scripts = repo / "scripts"
            scripts.mkdir(parents=True)
            (scripts / "v2-single-mac-upgrade.sh").write_text("#!/bin/bash\nexit 7\n")
            jobs = root / "jobs"
            fake = FakeLaunchctl(job_exit=7)
            exit_code = mod.run_upgrade_one_shot(
                repo_root=repo,
                domain=DOMAIN,
                launchctl="/bin/launchctl",
                job_dir=jobs,
                environment={"PATH": "/usr/bin", "HOME": str(root)},
                timeout_secs=5,
                runner=fake,
                sleep=lambda _seconds: None,
                now=lambda: 1234.0,
                token_hex=lambda _count: "deadbeef",
            )
            self.assertEqual(exit_code, 7)
            self.assertEqual(fake.loaded, {})
            self.assertEqual(len(fake.plists), 1)
            plist = fake.plists[0]
            self.assertIs(plist["RunAtLoad"], True)
            self.assertIs(plist["KeepAlive"], False)
            self.assertEqual(plist["ProgramArguments"][0], "/bin/bash")
            self.assertFalse(any(jobs.glob("*.plist")))
            bootstrap_calls = [call for call in fake.calls if call[1] == "bootstrap"]
            bootout_calls = [call for call in fake.calls if call[1] == "bootout"]
            self.assertEqual(len(bootstrap_calls), 1)
            self.assertEqual(len(bootout_calls), 1)
            self.assertFalse(any(call[1] == "kickstart" for call in fake.calls))


if __name__ == "__main__":
    unittest.main()
