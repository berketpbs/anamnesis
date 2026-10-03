"""Regressions use development cases only; no evaluation-answer tuning."""
import copy
import datetime as dt
import json
import tempfile
import threading
import unittest
from collections import Counter
from pathlib import Path

import comparison as c
from budget import Budget


class Experiment(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "experiment"
        self.manifest = c.prepare(self.root, "claude-pinned", "codex-pinned", split="development")

    def tearDown(self):
        self.tmp.cleanup()

    def evidence(self, trial, **overrides):
        result = dict(reader=trial["reader"], model=trial["model"], status="complete",
                      elapsed_seconds=100, preparation_seconds=10, tool_calls=3,
                      user_reminders=0, tokens=None, preparation_tokens=None, agent_cost_usd=None)
        result.update(overrides)
        return result

    def complete(self, trial, correct, **overrides):
        case = next(case for case in self.manifest["scenarios"] if case["id"] == trial["scenario"])
        artifact = case["expected"] if correct else dict(backend=case["expected"]["rejected"])
        (self.root / trial["repo"] / "policy.json").write_text(json.dumps(artifact), encoding="utf-8")
        return c.record(self.root, trial["id"], self.evidence(trial, **overrides))

    def test_schedule_separates_splits_and_freezes_shared_inputs(self):
        # Validate evaluation topology, not evaluation answers.
        evaluation = c.cases("evaluation")
        self.assertEqual(len(evaluation), 30)
        self.assertEqual(Counter(case["direction"] for case in evaluation),
                         {"claude-to-codex": 15, "codex-to-claude": 15})
        self.assertFalse({x["id"] for x in evaluation} & {x["id"] for x in self.manifest["scenarios"]})
        self.assertEqual(len(self.manifest["trials"]), 6 * 3 * 5)
        for case in self.manifest["scenarios"]:
            trials = [t for t in self.manifest["trials"] if t["scenario"] == case["id"]]
            for field in ("model", "prompt", "archive_sha256", "memory_sha256", "permissions", "budget"):
                self.assertTrue(all(t[field] == trials[0][field] for t in trials))
            self.assertEqual(len({t["profile"] for t in trials}), 15)
        with self.assertRaises(ValueError):
            c.prepare(self.root, "x", "y")

    def test_cutoff_refuses_future_answers_and_manifest_drift(self):
        items = copy.deepcopy(c.cases("development"))
        items[0]["archive"][0]["at"] = items[0]["task_at"]
        with self.assertRaises(ValueError):
            c.validate_cases(items)
        altered = dict(self.manifest, models=dict(claude="different", codex="codex-pinned"))
        (self.root / "manifest.json").write_text(json.dumps(altered), encoding="utf-8")
        with self.assertRaises(ValueError):
            c.load(self.root)

    def test_incomplete_quota_costs_and_denominator_are_visible(self):
        trial = self.manifest["trials"][0]
        self.complete(trial, True, status="quota", agent_cost_usd=.04)
        report = c.report(self.root)
        arm = report["arms"][trial["method"]]
        self.assertEqual(arm["planned"], 30)
        self.assertEqual(arm["completed"], 0)
        self.assertEqual(arm["incomplete"], 30)
        self.assertEqual(arm["agent_cost_known_usd"], .04)
        self.assertEqual(arm["statuses"]["quota"], 1)
        self.assertIn("incomplete", report["gate"])

    def test_completion_is_graded_and_trace_claims_need_evidence(self):
        trial = self.manifest["trials"][0]
        with self.assertRaises(ValueError):
            c.record(self.root, trial["id"], self.evidence(trial, model="gemini-3.6-flash"))
        with self.assertRaises(ValueError):
            c.record(self.root, trial["id"], self.evidence(trial, shown=True))
        with self.assertRaises(ValueError):
            c.record(self.root, trial["id"], self.evidence(trial, shown=True, shown_evidence="missing.log"))
        with self.assertRaises(ValueError):
            c.record(self.root, trial["id"], self.evidence(trial, shown=True, shown_evidence="../../../manifest.json"))
        result = self.complete(trial, False)
        self.assertFalse(result["passed"])
        self.assertTrue(result["wrong_decision"])
        with self.assertRaises(FileExistsError):
            self.complete(trial, True)

    def test_stage_claims_keep_a_trace_hash_and_graded_artifact(self):
        trial = self.manifest["trials"][0]
        trace = self.root / "trials" / trial["id"] / "delivery.log"
        trace.write_text("reader received the stored decision", encoding="utf-8")
        result = self.complete(trial, True, shown=True, shown_evidence="delivery.log")
        self.assertEqual(result["trace_sha256"]["shown"], c.hashlib.sha256(trace.read_bytes()).hexdigest())
        self.assertEqual(result["artifact"]["backend"], next(
            case["expected"]["backend"] for case in self.manifest["scenarios"] if case["id"] == trial["scenario"]))

    def test_repeats_stay_in_scenario_clusters_and_missing_tokens_stay_unknown(self):
        self.assertEqual(c.cluster_interval([.2] * 6), [.2, .2])
        for trial in self.manifest["trials"]:
            self.complete(trial, trial["method"] == "anamnesis")
        report = c.report(self.root)
        self.assertEqual(report["comparisons"]["brief"]["success_delta_ci95"], [1, 1])
        self.assertIsNone(report["comparisons"]["brief"]["tokens_ratio_ci95"])
        self.assertEqual(report["gate"], "development only; not an evaluation gate")

    def test_history_and_local_memory_are_read_only_inputs(self):
        trial = self.manifest["trials"][0]
        (self.root / trial["repo"] / "archive/history.jsonl").write_text("", encoding="utf-8")
        with self.assertRaises(ValueError):
            c.record(self.root, trial["id"], self.evidence(trial))

    def test_development_and_replacement_runs_share_the_total_budget(self):
        ledger = Budget(Path(self.manifest["budget_ledger"]), self.manifest["pricing"])
        try:
            ledger.reserve("gemini-3.6-flash", 0, 12_000_000, "preparation", today=dt.date(2026, 10, 3))
            replacement = c.prepare(self.root.parent / "replacement", "claude-pinned", "codex-pinned",
                                    split="development")
            self.assertEqual(replacement["budget_ledger"], self.manifest["budget_ledger"])
            second = Budget(Path(replacement["budget_ledger"]), replacement["pricing"])
            try:
                with self.assertRaises(ValueError):
                    second.reserve("gemini-3.6-flash", 0, 2_000_000, "retry", today=dt.date(2026, 10, 3))
            finally:
                second.close()
            self.assertEqual(c.report(self.root.parent / "replacement")["gemini_cost"]["remaining_usd"], 5)
        finally:
            ledger.close()


class BudgetTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / "budget.sqlite"
        self.pricing = dict(model="gemini-3.6-flash", input_usd_per_million="0.75",
                            output_usd_per_million="3.75", includes_thinking=True,
                            service_tier="standard", expires_on="2026-12-31")
        self.ledger = Budget(self.path, self.pricing)

    def tearDown(self):
        self.ledger.close()
        self.tmp.cleanup()

    def reserve(self, inputs, outputs, purpose="preparation"):
        return self.ledger.reserve("gemini-3.6-flash", inputs, outputs, purpose, today=dt.date(2026, 10, 3))

    def test_prep_retry_and_unknown_usage_share_one_cap_across_restarts(self):
        identity = self.reserve(1_000_000, 1_000_000)
        self.assertEqual(self.ledger.report()["unresolved_reserved_usd"], 4.5)
        self.ledger.settle(identity, "gemini-3.6-flash", 1000, 2000)
        self.assertEqual(self.ledger.report()["known_usd"], .00825)
        self.reserve(0, 12_000_000, "retry")
        other = Budget(self.path, self.pricing)
        try:
            with self.assertRaises(ValueError):
                other.reserve("gemini-3.6-flash", 0, 2_000_000, "summary", today=dt.date(2026, 10, 3))
            self.assertFalse(other.report()["cost_complete"])
        finally:
            other.close()

    def test_expired_prices_fallback_and_invalid_counts_cannot_dispatch(self):
        with self.assertRaises(ValueError):
            self.ledger.reserve("gemini-other", 1, 1, "brief")
        with self.assertRaises(ValueError):
            self.ledger.reserve("gemini-3.6-flash", 1, 1, "brief", today=dt.date(2027, 1, 1))
        with self.assertRaises(ValueError):
            self.reserve(-1, 1)
        with self.assertRaises(ValueError):
            Budget(self.path, dict(self.pricing, model="other"))

    def test_concurrent_reservations_do_not_overspend(self):
        outcomes = []
        def worker():
            ledger = Budget(self.path, self.pricing)
            try:
                ledger.reserve("gemini-3.6-flash", 0, 8_000_000, "summary", today=dt.date(2026, 10, 3))
                outcomes.append("reserved")
            except ValueError:
                outcomes.append("stopped")
            finally:
                ledger.close()
        threads = [threading.Thread(target=worker) for _ in range(2)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        self.assertEqual(sorted(outcomes), ["reserved", "stopped"])
        self.assertEqual(self.ledger.report()["unresolved_reserved_usd"], 30)

    def test_overrun_records_the_actual_bill_and_stops_further_dispatch(self):
        identity = self.reserve(1, 1)
        with self.assertRaises(ValueError):
            self.ledger.settle(identity, "gemini-3.6-flash", 100, 100)
        self.assertEqual(self.ledger.report()["known_usd"], .00045)
        with self.assertRaises(ValueError):
            self.reserve(1, 1)


if __name__ == "__main__":
    unittest.main()
