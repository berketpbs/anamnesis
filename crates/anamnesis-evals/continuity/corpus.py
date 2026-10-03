"""Import privately supplied, frozen tasks; provenance still needs human review."""
import datetime as dt
import hashlib
import json
from collections import Counter
from pathlib import Path, PurePosixPath


def relative_file(value):
    if not isinstance(value, str) or not value or "\\" in value or ":" in value:
        raise ValueError("file path must be a portable relative path")
    path = PurePosixPath(value)
    if path.is_absolute() or any(part in (".", "..") for part in value.split("/")):
        raise ValueError("file path escapes the task workspace")
    return path


def validate_check(check):
    relative_file(check["file"])
    if check["kind"] == "json-equals":
        if not isinstance(check.get("keys"), list) or any(not isinstance(k, (str, int)) for k in check["keys"]):
            raise ValueError("JSON check needs a list of object keys or array indices")
        if "value" not in check:
            raise ValueError("JSON check needs an expected value")
    elif check["kind"] == "contains":
        if not isinstance(check.get("text"), str) or not check["text"]:
            raise ValueError("text check needs nonempty text")
    elif check["kind"] != "absent":
        raise ValueError("unsupported task check; no arbitrary shell checker is executed")


def import_corpus(path: Path, split: str):
    value = json.loads(path.read_text(encoding="utf-8"))
    if value.get("format") != 1 or value.get("kind") != "captured-developer-tasks":
        raise ValueError("expected a captured-developer-tasks corpus, format 1")
    items = value["scenarios"]
    ids, task_hashes, source_sets = set(), set(), set()
    for case in items:
        identity = case["id"]
        if not isinstance(identity, str) or not identity or not identity.replace("-", "").replace("_", "").isalnum():
            raise ValueError("scenario ID must contain letters, digits, hyphens or underscores")
        if identity in ids:
            raise ValueError("duplicate scenario ID")
        ids.add(identity)
        writer, reader = {"claude-to-codex": ("claude", "codex"),
                          "codex-to-claude": ("codex", "claude")}[case["direction"]]
        if (case["writer"], case["reader"]) != (writer, reader):
            raise ValueError("reader/writer differs from handoff direction")
        if case["split"] not in ("development", "evaluation"):
            raise ValueError("invalid corpus split")
        if not isinstance(case.get("prompt"), str) or not case["prompt"].strip():
            raise ValueError("a task-specific continuation prompt is required")
        seed = case["seed"]
        if not isinstance(seed, dict) or not seed:
            raise ValueError("task needs an initial repository snapshot")
        for name, text in seed.items():
            file = relative_file(name)
            if file.parts[0] in ("archive", "local-memory", ".codex", ".claude", ".git"):
                raise ValueError("seed may not replace archive, memory or client configuration")
            if not isinstance(text, str):
                raise ValueError("seed files must be UTF-8 text")
        task_hash = hashlib.sha256(json.dumps(dict(prompt=case["prompt"], seed=seed),
                                              sort_keys=True).encode()).hexdigest()
        if task_hash in task_hashes:
            raise ValueError("repeated task snapshot/prompt; ID variants are not distinct scenarios")
        task_hashes.add(task_hash)
        checks = case["checks"]
        if not checks:
            raise ValueError("task needs artifact acceptance checks")
        for check in checks + [case["decision_check"], case["stale_decision_check"]]:
            validate_check(check)
        cutoff = dt.datetime.fromisoformat(case["task_at"])
        if cutoff.tzinfo is None:
            raise ValueError("task cutoff needs a timezone")
        archive = case["archive"]
        if not archive:
            raise ValueError("captured history is missing")
        event_ids = set()
        for event in archive:
            at = dt.datetime.fromisoformat(event["at"])
            if at.tzinfo is None or at >= cutoff:
                raise ValueError("history contains messages at or after the task cutoff")
            if event["speaker"] not in ("user", "assistant") or event["agent"] != writer:
                raise ValueError("history must identify the original writer and message role")
            if not isinstance(event["text"], str) or not event.get("source_session") or not event.get("source_event"):
                raise ValueError("captured event text and original session/event IDs are required")
            pair = (event["source_session"], event["source_event"])
            if pair in event_ids:
                raise ValueError("duplicate captured source event")
            event_ids.add(pair)
        sources = tuple(sorted(event_ids))
        if sources in source_sets:
            raise ValueError("reused source history is not an independent scenario")
        source_sets.add(sources)
    for group, required, each in (("development", 6, 3), ("evaluation", 30, 15)):
        selected = [case for case in items if case["split"] == group]
        if len(selected) != required or Counter(c["direction"] for c in selected) != {
                "claude-to-codex": each, "codex-to-claude": each}:
            raise ValueError(f"corpus needs {required} distinct {group} cases, balanced by direction")
    return [c for c in items if c["split"] == split], dict(
        kind="externally-supplied-v1", sha256=hashlib.sha256(path.read_bytes()).hexdigest(),
        review_status="unverified; review source provenance, task independence and acceptance checks")


def check_artifact(repo: Path, check):
    """Read only an in-scope artifact; a task cannot grade an escaped symlink."""
    validate_check(check)
    target = (repo / str(relative_file(check["file"]))).resolve()
    if not target.is_relative_to(repo.resolve()):
        return False
    if check["kind"] == "absent":
        return not target.exists()
    try:
        text = target.read_text(encoding="utf-8")
        if check["kind"] == "contains":
            return check["text"] in text
        actual = json.loads(text)
        for key in check["keys"]:
            actual = actual[key]
        return type(actual) is type(check["value"]) and actual == check["value"]
    except (OSError, ValueError, KeyError, IndexError, TypeError):
        return False
