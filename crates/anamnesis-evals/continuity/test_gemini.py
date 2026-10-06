import json
import tempfile
import unittest
from pathlib import Path

import comparison as c
from budget import Budget
from gemini import generate


class GeminiAdapterTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "run"
        self.manifest = c.prepare(self.root, "claude", "codex", split="development")
        self.output = self.root / "preparation" / "durum.md"

    def tearDown(self):
        self.tmp.cleanup()

    def costs(self):
        ledger = Budget(Path(self.manifest["budget_ledger"]), self.manifest["pricing"])
        try:
            return ledger.report()
        finally:
            ledger.close()

    def transport(self, url, key, body=None):
        if body is None:
            return dict(name="models/gemini-3.6-flash", inputTokenLimit=1048576, outputTokenLimit=65536)
        self.assertEqual(self.costs()["requests"], 1)
        self.assertGreater(self.costs()["unresolved_reserved_usd"], 0)
        self.assertEqual(body["generationConfig"]["candidateCount"], 1)
        self.assertNotIn(key, url)
        return dict(modelVersion="gemini-3.6-flash", usageMetadata=dict(promptTokenCount=100,
            candidatesTokenCount=20, thoughtsTokenCount=30, totalTokenCount=150),
            candidates=[dict(finishReason="STOP", content=dict(parts=[dict(text="retained decision")]))])

    def test_reserves_before_dispatch_and_charges_thinking_without_key_in_trace(self):
        result = generate(self.root, "history", self.output, "brief", 4096,
                          key="private-test-key", transport=self.transport)
        self.assertTrue(result["complete"])
        self.assertEqual(result["output_tokens_including_thinking"], 50)
        self.assertEqual(self.costs()["known_usd"], .000263)
        self.assertEqual(self.costs()["unresolved_reserved_usd"], 0)
        for path in self.output.parent.iterdir():
            self.assertNotIn("private-test-key", path.read_text(encoding="utf-8"))
        with self.assertRaises(ValueError):
            generate(self.root, "history", self.output, "retry", 4096,
                     key="private-test-key", transport=self.transport)

    def test_failed_request_keeps_hold_and_missing_usage_is_not_zero_cost(self):
        def fail(url, key, body=None):
            if body is None:
                return self.transport(url, key)
            raise OSError("simulated timeout")
        with self.assertRaises(OSError):
            generate(self.root, "history", self.output, "summary", 4096, key="test", transport=fail)
        self.assertEqual(self.costs()["requests"], 1)
        self.assertFalse(self.costs()["cost_complete"])
        other = self.output.with_name("retry.md")
        def missing(url, key, body=None):
            return self.transport(url, key) if body is None else dict(modelVersion="gemini-3.6-flash", candidates=[])
        with self.assertRaises(KeyError):
            generate(self.root, "history", other, "retry", 4096, key="test", transport=missing)
        self.assertEqual(self.costs()["requests"], 2)
        self.assertEqual(self.costs()["known_usd"], 0)

    def test_wrong_response_model_retains_hold_and_exhausted_budget_never_generates(self):
        def wrong(url, key, body=None):
            if body is None:
                return self.transport(url, key)
            return dict(modelVersion="unexpected-fallback")
        with self.assertRaises(ValueError):
            generate(self.root, "history", self.output, "access-check", 32, key="test", transport=wrong)
        self.assertFalse(self.costs()["cost_complete"])
        ledger = Budget(Path(self.manifest["budget_ledger"]), self.manifest["pricing"])
        try:
            ledger.reserve("gemini-3.6-flash", 64_000_000, 0, "preparation")
        finally:
            ledger.close()
        def forbid_generation(url, key, body=None):
            self.assertIsNone(body, "budget exhausted but generation dispatched")
            return self.transport(url, key)
        with self.assertRaises(ValueError):
            generate(self.root, "history", self.output.with_name("blocked.md"), "brief", 32,
                     key="test", transport=forbid_generation)
