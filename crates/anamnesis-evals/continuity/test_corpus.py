"""Importer contract tests use generated inputs, never held-out task answers."""
import copy
import json
import tempfile
import unittest
from pathlib import Path

import comparison as c
from corpus import import_corpus, check_artifact


def specimen():
    items = []
    for split, count in (("development", 6), ("evaluation", 30)):
        for i in range(count):
            name = f"{split}-{i}"
            direction = "claude-to-codex" if i % 2 == 0 else "codex-to-claude"
            writer, reader = ("claude", "codex") if i % 2 == 0 else ("codex", "claude")
            decision = dict(kind="json-equals", file="result.json", keys=["chosen"], value=name)
            items.append(dict(id=name, split=split, direction=direction, writer=writer, reader=reader,
                workstream=name, category="contract-test", prompt=f"Continue contract test {name}.",
                task_at="2026-09-03T00:00:00+00:00", seed={"result.json": "{}\n"},
                archive=[dict(id=name, source_session=f"session-{name}", source_event=f"event-{name}",
                    agent=writer, speaker="user", at="2026-09-02T00:00:00+00:00", text=f"Choose {name}.")],
                checks=[decision, dict(kind="contains", file="next.txt", text="verify restore")],
                decision_check=decision,
                stale_decision_check=dict(kind="json-equals", file="result.json", keys=["chosen"], value="stale")))
    return dict(format=1, kind="captured-developer-tasks", scenarios=items)


class CorpusTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / "corpus.json"
        self.value = specimen()

    def tearDown(self):
        self.tmp.cleanup()

    def load(self, value=None):
        self.path.write_text(json.dumps(value or self.value), encoding="utf-8")
        return import_corpus(self.path, "development")

    def test_import_freezes_same_inputs_and_checks_produced_task_artifacts(self):
        selected, info = self.load()
        self.assertEqual(len(selected), 6)
        self.assertIn("unverified", info["review_status"])
        root = Path(self.tmp.name) / "run"
        manifest = c.prepare(root, "claude", "codex", split="development", corpus_path=self.path)
        trial = manifest["trials"][0]
        repo = root / trial["repo"]
        (repo / "result.json").write_text(json.dumps(dict(chosen=trial["scenario"])), encoding="utf-8")
        (repo / "next.txt").write_text("verify restore", encoding="utf-8")
        evidence = dict(reader=trial["reader"], model=trial["model"], status="complete", elapsed_seconds=1,
                        preparation_seconds=0, tool_calls=1, user_reminders=0)
        outcome = c.record(root, trial["id"], evidence)
        self.assertTrue(outcome["passed"])
        self.assertTrue(outcome["decision_correct"])
        self.assertFalse(outcome["wrong_decision"])
        self.assertEqual(set(outcome["artifact_sha256"]), {"result.json", "next.txt"})
        self.assertIn("incomplete", c.report(root)["gate"])

    def test_rejects_cutoff_duplicates_id_variants_and_path_escapes(self):
        mutations = [
            lambda v: v["scenarios"][0]["archive"][0].update(at=v["scenarios"][0]["task_at"]),
            lambda v: v["scenarios"][0]["seed"].update({"../gold.json": "secret"}),
            lambda v: v["scenarios"][0]["seed"].update({".codex/config.toml": "unsafe"}),
            lambda v: v["scenarios"][0].update(prompt=v["scenarios"][1]["prompt"], seed=v["scenarios"][1]["seed"]),
            lambda v: v["scenarios"][0]["archive"][0].update(source_session=None),
            lambda v: v["scenarios"][0].update(direction="codex-to-claude"),
            lambda v: v["scenarios"][0]["checks"][0].update(kind="shell"),
            lambda v: v["scenarios"].pop(),
        ]
        for mutate in mutations:
            value = copy.deepcopy(self.value)
            mutate(value)
            with self.subTest(mutation=mutate), self.assertRaises((ValueError, KeyError)):
                self.load(value)

    def test_missing_wrong_and_escaped_artifacts_do_not_pass(self):
        repo = Path(self.tmp.name) / "repo"
        repo.mkdir()
        check = dict(kind="json-equals", file="result.json", keys=["nested", 0], value=True)
        self.assertFalse(check_artifact(repo, check))
        (repo / "result.json").write_text('{"nested":[1]}', encoding="utf-8")
        self.assertFalse(check_artifact(repo, check))
        (repo / "result.json").write_text('{"nested":[true]}', encoding="utf-8")
        self.assertTrue(check_artifact(repo, check))
        with self.assertRaises(ValueError):
            check_artifact(repo, dict(check, file="../corpus.json"))
