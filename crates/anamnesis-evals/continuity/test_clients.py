import datetime as dt
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import comparison as c
from clients import client_plan, execute, StreamMeasurements


class ClientTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name) / "run"
        self.manifest = c.prepare(self.root, "claude-pinned", "codex-pinned", split="development")

    def tearDown(self):
        self.tmp.cleanup()

    def trial(self, reader, method="history"):
        return next(t for t in self.manifest["trials"] if t["reader"] == reader and t["method"] == method)

    def test_profiles_and_provider_variables_are_isolated_without_changing_host(self):
        trial = self.trial("codex")
        with patch.dict(os.environ, {"OPENAI_API_KEY": "secret", "ANAMNESIS_LLM_PROVIDER": "google",
                                     "CODEX_HOME": "host-profile"}):
            plan = client_plan(self.root, trial["id"], "codex")
            self.assertNotIn("OPENAI_API_KEY", plan["env"])
            self.assertEqual(plan["env"]["ANAMNESIS_LLM_PROVIDER"], "none")
            self.assertTrue(Path(plan["env"]["CODEX_HOME"]).is_relative_to(self.root))
            self.assertEqual(os.environ["CODEX_HOME"], "host-profile")
        self.assertIn("--no-daemon", plan["argv"])
        self.assertIn("--ignore-user-config", plan["argv"])
        self.assertNotIn("mcp_servers.anamnesis.required=true", plan["argv"])
        with self.assertRaises(ValueError):
            client_plan(self.root, trial["id"], "codex", anamnesis=Path("anamnesis.exe"))
        brief = self.trial("codex", "brief")
        with self.assertRaises(ValueError):
            client_plan(self.root, brief["id"], "codex")

    def test_claude_pause_and_missing_readiness_refuse_before_starting_process(self):
        trial = self.trial("claude")
        plan = client_plan(self.root, trial["id"], "claude")
        ready = dict(reader=trial["reader"], model=trial["model"], access_verified=True,
                     profile_isolation_verified=True, permissions_verified=True,
                     treatment_verified=True, evidence="private fresh-process verification")
        with self.assertRaises(ValueError):
            execute(plan, claude_ready=True, readiness=ready,
                    now=dt.datetime(2026, 10, 3, 15, 59, tzinfo=dt.timezone.utc))
        with self.assertRaises(ValueError):
            execute(plan, readiness=ready, now=dt.datetime(2026, 10, 3, 16, tzinfo=dt.timezone.utc))
        codex = client_plan(self.root, self.trial("codex")["id"], "codex")
        with self.assertRaises(ValueError):
            execute(codex)
        self.assertFalse((plan["cwd"].parent / "dispatch.json").exists())

    def test_stream_accounting_deduplicates_actions_and_preserves_unknown_cost(self):
        measured = StreamMeasurements("codex")
        for kind in ("item.started", "item.completed"):
            measured.consume(json.dumps(dict(type=kind, item=dict(id="action-1", type="command_execution"))))
        measured.consume(json.dumps(dict(type="turn.completed", usage=dict(input_tokens=100, output_tokens=20))))
        self.assertEqual(len(measured.actions), 1)
        self.assertEqual(measured.tokens, 120)
        self.assertIsNone(measured.cost)
        claude = StreamMeasurements("claude")
        claude.consume(json.dumps(dict(type="result", subtype="success", total_cost_usd=.1,
            usage=dict(input_tokens=20, output_tokens=10, cache_read_input_tokens=100))))
        self.assertEqual(claude.tokens, 130)
        self.assertTrue(claude.complete)

    def test_owned_fake_client_produces_real_process_measurements_without_an_api(self):
        trial = self.trial("codex")
        plan = client_plan(self.root, trial["id"], sys.executable)
        fake = self.root / "fake-client.py"
        fake.write_text('import json,sys\nsys.stdin.read()\nprint(json.dumps({"type":"turn.completed",'
                        '"usage":{"input_tokens":10,"output_tokens":5}}))\n', encoding="utf-8")
        plan["argv"] = [sys.executable, str(fake)]
        ready = dict(reader="codex", model="codex-pinned", access_verified=True,
                     profile_isolation_verified=True, permissions_verified=True,
                     treatment_verified=True, evidence="fake-client contract test only")
        result = execute(plan, readiness=ready)
        self.assertEqual(result["status"], "complete")
        self.assertEqual(result["tokens"], 15)
        self.assertIsNone(result["shown"])
        self.assertIsNone(result["preparation_seconds"])
        self.assertIsNone(result["user_reminders"])
        with self.assertRaises(ValueError):
            c.record(self.root, trial["id"], result)
        with self.assertRaises(FileExistsError):
            execute(plan, readiness=ready)

    def test_wall_budget_stops_an_owned_process_and_preserves_incomplete_measurements(self):
        trial = self.trial("codex")
        plan = client_plan(self.root, trial["id"], sys.executable)
        fake = self.root / "slow-client.py"
        fake.write_text("import sys,time\nsys.stdin.read()\ntime.sleep(30)\n", encoding="utf-8")
        plan["argv"] = [sys.executable, str(fake)]
        plan["trial"]["budget"]["seconds"] = .2
        ready = dict(reader="codex", model="codex-pinned", access_verified=True,
                     profile_isolation_verified=True, permissions_verified=True,
                     treatment_verified=True, evidence="fake-client timeout test only")
        result = execute(plan, readiness=ready)
        self.assertEqual(result["status"], "budget")
        self.assertLess(result["elapsed_seconds"], 15)
        self.assertIsNone(result["tokens"])
