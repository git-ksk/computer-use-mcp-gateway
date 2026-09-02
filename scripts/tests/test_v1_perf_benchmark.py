import importlib.util
import pathlib
import sys
import unittest
from unittest import mock

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "v1_perf_benchmark.py"
SPEC = importlib.util.spec_from_file_location("v1_perf_benchmark", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)


class V1PerfBenchmarkTests(unittest.TestCase):
    def test_nearest_rank_percentiles_are_deterministic(self):
        values = [float(i) for i in range(1, 101)]
        self.assertEqual(mod.percentile(values, 50), 50.0)
        self.assertEqual(mod.percentile(values, 95), 95.0)
        self.assertEqual(mod.percentile(values, 99), 99.0)

    def test_summary_reports_required_distribution_and_throughput(self):
        summary = mod.summarize([1.0, 2.0, 3.0, 4.0], 2.0, 4)
        self.assertEqual(summary["calls"], 4)
        self.assertEqual(summary["concurrency"], 4)
        self.assertEqual(summary["throughput_rps"], 2.0)
        for key in ["mean_ms", "p50_ms", "p95_ms", "p99_ms", "max_ms"]:
            self.assertIn(key, summary)

    def test_default_profiles_are_1_4_16(self):
        args = mod.parse_args([])
        self.assertEqual(args.concurrency, [1, 4, 16])
        self.assertEqual(args.calls, 1000)
        self.assertEqual(args.warmup, 50)

    def test_concurrent_runner_returns_one_latency_per_call(self):
        with mock.patch.object(mod, "timed_call", side_effect=lambda _url, request_id: float(request_id)):
            values = mod.run_calls("http://127.0.0.1:1", 8, 4, 100)
        self.assertEqual(len(values), 8)
        self.assertEqual(sorted(values), [float(i) for i in range(100, 108)])

    def test_non_linux_process_resources_are_explicitly_unavailable(self):
        self.assertIsNone(mod.linux_process_resources(999999, platform="darwin"))

    def test_invalid_concurrency_fails_parser(self):
        with self.assertRaises(Exception):
            mod.parse_concurrencies("1,0,4")


if __name__ == "__main__":
    unittest.main()
