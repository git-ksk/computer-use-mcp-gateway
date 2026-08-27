import importlib.util
import pathlib
import subprocess
import sys
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "v2_launchd_topology_guard.py"
SPEC = importlib.util.spec_from_file_location("v2_launchd_topology_guard", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)

GITHUB_HUB = "com.github.git-ksk.cumg-v2-hub"
GITHUB_AGENT = "com.github.git-ksk.cumg-v2-agent"
SAWADA_HUB = "com.sawadakousuke.cumg-v2-hub"
SAWADA_AGENT = "com.sawadakousuke.cumg-v2-agent"
DOMAIN = "gui/501"


class FakeLaunchctl:
    def __init__(self, loaded=(), fail_disable=()):
        self.loaded = set(loaded)
        self.disabled = set()
        self.fail_disable = set(fail_disable)
        self.calls = []

    def __call__(self, argv, *, stdout, stderr, check):
        self.calls.append(tuple(argv))
        action = argv[1]
        target = argv[2]
        label = target.split("/", 2)[-1]
        if action == "print":
            code = 0 if label in self.loaded else 113
        elif action == "bootout":
            if label in self.loaded:
                self.loaded.remove(label)
                code = 0
            else:
                code = 113
        elif action == "disable":
            if label in self.fail_disable:
                code = 1
            else:
                self.disabled.add(label)
                code = 0
        else:
            raise AssertionError(f"unexpected action {action}")
        return subprocess.CompletedProcess(argv, code)


class LaunchdTopologyGuardTests(unittest.TestCase):
    def inspect(self, fake, hub=GITHUB_HUB, agent=GITHUB_AGENT):
        return mod.inspect_topology(
            domain=DOMAIN,
            hub_label=hub,
            agent_label=agent,
            launchctl="/bin/launchctl",
            runner=fake,
        )

    def test_single_configured_family_is_accepted(self):
        fake = FakeLaunchctl((GITHUB_HUB, GITHUB_AGENT))
        topology = self.inspect(fake)
        self.assertEqual(topology.hub_loaded, (GITHUB_HUB,))
        self.assertEqual(topology.agent_loaded, (GITHUB_AGENT,))
        self.assertTrue(all(call[1] == "print" for call in fake.calls))

    def test_dual_hub_family_is_refused(self):
        fake = FakeLaunchctl((GITHUB_HUB, SAWADA_HUB, GITHUB_AGENT))
        with self.assertRaises(mod.GuardError) as raised:
            self.inspect(fake)
        self.assertEqual(raised.exception.reason, "conflicting_launchd_labels")
        self.assertIn("role=hub", raised.exception.details)
        self.assertIn(GITHUB_HUB, raised.exception.details)
        self.assertIn(SAWADA_HUB, raised.exception.details)

    def test_dual_agent_family_is_refused(self):
        fake = FakeLaunchctl((GITHUB_HUB, GITHUB_AGENT, SAWADA_AGENT))
        with self.assertRaises(mod.GuardError) as raised:
            self.inspect(fake)
        self.assertEqual(raised.exception.reason, "conflicting_launchd_labels")
        self.assertIn("role=agent", raised.exception.details)
        self.assertIn(GITHUB_AGENT, raised.exception.details)
        self.assertIn(SAWADA_AGENT, raised.exception.details)

    def test_cross_role_mixed_family_is_refused(self):
        fake = FakeLaunchctl((GITHUB_HUB, SAWADA_AGENT))
        with self.assertRaisesRegex(mod.GuardError, "mixed_launchd_families"):
            self.inspect(fake)

    def test_configured_mixed_family_is_refused_even_when_unloaded(self):
        fake = FakeLaunchctl()
        with self.assertRaisesRegex(mod.GuardError, "configured_launchd_family_mismatch"):
            self.inspect(fake, hub=GITHUB_HUB, agent=SAWADA_AGENT)
        self.assertEqual(fake.calls, [])

    def test_retire_alternates_boots_out_and_disables_without_touching_configured_labels(self):
        fake = FakeLaunchctl((SAWADA_HUB, SAWADA_AGENT))
        retired = mod.retire_alternates(
            domain=DOMAIN,
            hub_label=GITHUB_HUB,
            agent_label=GITHUB_AGENT,
            launchctl="/bin/launchctl",
            runner=fake,
        )
        self.assertEqual(set(retired), {SAWADA_HUB, SAWADA_AGENT})
        self.assertEqual(fake.loaded, set())
        self.assertEqual(fake.disabled, {SAWADA_HUB, SAWADA_AGENT})
        flattened = "\n".join(" ".join(call) for call in fake.calls)
        self.assertNotIn(f"disable {DOMAIN}/{GITHUB_HUB}", flattened)
        self.assertNotIn(f"disable {DOMAIN}/{GITHUB_AGENT}", flattened)

    def test_retire_alternates_disables_unloaded_old_family_to_prevent_restart(self):
        fake = FakeLaunchctl()
        mod.retire_alternates(
            domain=DOMAIN,
            hub_label=SAWADA_HUB,
            agent_label=SAWADA_AGENT,
            launchctl="/bin/launchctl",
            runner=fake,
        )
        self.assertEqual(fake.disabled, {GITHUB_HUB, GITHUB_AGENT})


    def test_upgrade_sequence_retires_alternate_family_and_returns_to_one_configured_pair(self):
        fake = FakeLaunchctl((GITHUB_HUB, GITHUB_AGENT))
        initial = self.inspect(fake)
        self.assertEqual(initial.hub_loaded, (GITHUB_HUB,))
        self.assertEqual(initial.agent_loaded, (GITHUB_AGENT,))

        # Simulate the reviewed drain/unload boundary, then a stale alternate family becoming
        # loaded before the new configured pair starts. Retirement must contain it first.
        fake.loaded.clear()
        fake.loaded.update((SAWADA_HUB, SAWADA_AGENT))
        mod.retire_alternates(
            domain=DOMAIN,
            hub_label=GITHUB_HUB,
            agent_label=GITHUB_AGENT,
            launchctl="/bin/launchctl",
            runner=fake,
        )
        self.assertEqual(fake.loaded, set())
        self.assertEqual(fake.disabled, {SAWADA_HUB, SAWADA_AGENT})

        fake.loaded.update((GITHUB_HUB, GITHUB_AGENT))
        final = self.inspect(fake)
        self.assertEqual(final.hub_loaded, (GITHUB_HUB,))
        self.assertEqual(final.agent_loaded, (GITHUB_AGENT,))

    def test_retire_alternates_fails_closed_when_disable_fails(self):
        fake = FakeLaunchctl(fail_disable=(SAWADA_AGENT,))
        with self.assertRaisesRegex(mod.GuardError, "alternate_launchd_disable_failed"):
            mod.retire_alternates(
                domain=DOMAIN,
                hub_label=GITHUB_HUB,
                agent_label=GITHUB_AGENT,
                launchctl="/bin/launchctl",
                runner=fake,
            )

    def test_invalid_label_and_domain_are_refused_before_launchctl(self):
        fake = FakeLaunchctl()
        with self.assertRaisesRegex(mod.GuardError, "invalid_launchd_label"):
            mod.inspect_topology(
                domain=DOMAIN,
                hub_label="bad label",
                agent_label=GITHUB_AGENT,
                launchctl="/bin/launchctl",
                runner=fake,
            )
        with self.assertRaisesRegex(mod.GuardError, "invalid_launchd_domain"):
            mod.inspect_topology(
                domain="system",
                hub_label=GITHUB_HUB,
                agent_label=GITHUB_AGENT,
                launchctl="/bin/launchctl",
                runner=fake,
            )
        self.assertEqual(fake.calls, [])


if __name__ == "__main__":
    unittest.main()
