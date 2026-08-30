import importlib.util
import os
import pathlib
import plistlib
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "v2_mutation_authority_preflight.py"
SPEC = importlib.util.spec_from_file_location("v2_mutation_authority_preflight", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)

DOMAIN = "gui/501"
LEGACY = mod.LEGACY_LABEL
AGENT = "com.github.git-ksk.cumg-v2-agent"


class FakeLaunchctl:
    def __init__(self, loaded=()):
        self.loaded = set(loaded)

    def __call__(self, argv, *, stdout, stderr, check):
        label = argv[2].split("/", 2)[-1]
        return subprocess.CompletedProcess(argv, 0 if label in self.loaded else 113)


class MutationAuthorityPreflightTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp.name)
        self.backend = self.root / "cua-driver"
        self.backend.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        self.backend.chmod(0o700)
        self.authority = self.root / "authority"
        self.authority.mkdir(mode=0o700)
        (self.authority / mod.LOCK_FILE).write_bytes(b"")
        (self.authority / mod.LOCK_FILE).chmod(0o600)
        (self.authority / mod.STATE_FILE).write_text(
            '{"schema_version":1,"owner":"v1","epoch":3}\n', encoding="utf-8"
        )
        (self.authority / mod.STATE_FILE).chmod(0o600)
        self.legacy_plist = self.root / "legacy.plist"
        self.agent_plist = self.root / "agent.plist"
        self.write_profiles(self.authority, self.authority)

    def tearDown(self):
        self.temp.cleanup()

    def write_profiles(self, legacy_authority, agent_authority):
        legacy = {
            "Label": LEGACY,
            "ProgramArguments": [
                str(self.root / "legacy-gateway"),
                "--backend-command",
                str(self.backend),
            ],
        }
        if legacy_authority is not None:
            legacy["ProgramArguments"] += ["--mutation-authority-dir", str(legacy_authority)]
        agent = {
            "Label": AGENT,
            "ProgramArguments": [str(self.root / "v2-agent")],
            "EnvironmentVariables": {"CUMG_V2_CUA_COMMAND": str(self.backend)},
        }
        if agent_authority is not None:
            agent["EnvironmentVariables"]["CUMG_MUTATION_AUTHORITY_DIR"] = str(agent_authority)
        with self.legacy_plist.open("wb") as stream:
            plistlib.dump(legacy, stream)
        with self.agent_plist.open("wb") as stream:
            plistlib.dump(agent, stream)

    def inspect(self, loaded):
        return mod.inspect_coexistence(
            domain=DOMAIN,
            launchctl="/bin/launchctl",
            legacy_label=LEGACY,
            agent_label=AGENT,
            legacy_plist=self.legacy_plist,
            agent_plist=self.agent_plist,
            runner=FakeLaunchctl(loaded),
        )

    def test_legacy_only_is_observable_without_claiming_v2_authority(self):
        legacy = self.inspect((LEGACY,))
        self.assertEqual(legacy, (True, False, False, None, None))

    def test_v2_only_requires_and_reports_initialized_authority(self):
        agent = self.inspect((AGENT,))
        self.assertEqual(agent[:3], (False, True, False))
        self.assertEqual(agent[3], self.authority.resolve())
        self.assertEqual(agent[4].owner, "v1")
        self.assertEqual(agent[4].epoch, 3)

    def test_v2_only_missing_authority_is_refused(self):
        self.write_profiles(self.authority, None)
        with self.assertRaises(mod.PreflightError) as raised:
            self.inspect((AGENT,))
        self.assertEqual(raised.exception.reason, "mutation_authority_missing")


    def test_v2_only_legacy_profile_can_enter_explicit_migration_lane(self):
        self.write_profiles(self.authority, None)
        result = mod.inspect_coexistence(
            domain=DOMAIN,
            launchctl="/bin/launchctl",
            legacy_label=LEGACY,
            agent_label=AGENT,
            legacy_plist=self.legacy_plist,
            agent_plist=self.agent_plist,
            allow_v2_uninitialized=True,
            runner=FakeLaunchctl((AGENT,)),
        )
        self.assertEqual(result, (False, True, False, None, None))

    def test_unfenced_legacy_blocks_v2_uninitialized_migration_lane(self):
        self.write_profiles(None, None)
        with self.assertRaises(mod.PreflightError) as raised:
            mod.inspect_coexistence(
                domain=DOMAIN,
                launchctl="/bin/launchctl",
                legacy_label=LEGACY,
                agent_label=AGENT,
                legacy_plist=self.legacy_plist,
                agent_plist=self.agent_plist,
                allow_v2_uninitialized=True,
                runner=FakeLaunchctl((LEGACY, AGENT)),
            )
        self.assertEqual(raised.exception.reason, "legacy_gateway_unfenced")

    def test_supported_coexistence_requires_same_authority_domain(self):
        legacy, agent, shared, authority, state = self.inspect((LEGACY, AGENT))
        self.assertTrue(legacy and agent and shared)
        self.assertEqual(authority, self.authority.resolve())
        self.assertEqual(state.owner, "v1")
        self.assertEqual(state.epoch, 3)

    def test_same_backend_without_shared_authority_is_refused(self):
        self.write_profiles(None, self.authority)
        with self.assertRaises(mod.PreflightError) as raised:
            self.inspect((LEGACY, AGENT))
        self.assertEqual(raised.exception.reason, "shared_mutation_authority_missing")
        self.assertIn("legacy_label=", raised.exception.details)
        self.assertIn("agent_label=", raised.exception.details)

    def test_mismatched_authority_domains_are_refused(self):
        other = self.root / "other-authority"
        other.mkdir(mode=0o700)
        self.write_profiles(self.authority, other)
        with self.assertRaisesRegex(mod.PreflightError, "shared_mutation_authority_mismatch"):
            self.inspect((LEGACY, AGENT))

    def test_malformed_or_broad_authority_state_is_refused(self):
        state = self.authority / mod.STATE_FILE
        state.write_text("{}\n", encoding="utf-8")
        state.chmod(0o600)
        with self.assertRaisesRegex(mod.PreflightError, "mutation_authority_invalid_state"):
            self.inspect((LEGACY, AGENT))
        state.write_text('{"schema_version":1,"owner":"v1","epoch":1}\n', encoding="utf-8")
        state.chmod(0o644)
        with self.assertRaisesRegex(mod.PreflightError, "mutation_authority_unsafe_permissions"):
            self.inspect((LEGACY, AGENT))

    def test_distinct_explicit_backends_do_not_claim_a_shared_authority_domain(self):
        other_backend = self.root / "other-cua-driver"
        other_backend.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        other_backend.chmod(0o700)
        with self.agent_plist.open("rb") as stream:
            agent = plistlib.load(stream)
        agent["EnvironmentVariables"]["CUMG_V2_CUA_COMMAND"] = str(other_backend)
        with self.agent_plist.open("wb") as stream:
            plistlib.dump(agent, stream)
        result = self.inspect((LEGACY, AGENT))
        self.assertEqual(result[:3], (True, True, False))
        self.assertEqual(result[3], self.authority.resolve())
        self.assertEqual(result[4].owner, "v1")


if __name__ == "__main__":
    unittest.main()
