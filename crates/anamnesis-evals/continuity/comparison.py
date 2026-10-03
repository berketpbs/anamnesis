#!/usr/bin/env python3
"""Prepare and score a three-arm continuity experiment without starting agents.

The generated corpus is a controlled fixture. Genuine client handoffs are a
separate acceptance gate. No API calls or user configuration changes occur here.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import itertools
import json
import math
import random
import statistics
from collections import Counter
from pathlib import Path

from budget import Budget
from corpus import import_corpus, check_artifact

METHODS = ("history", "brief", "anamnesis")
CATEGORIES = ("accepted", "rejected", "superseded", "failed-attempt", "long-middle", "parallel")
REPEATS = 5
PERMISSIONS = ["read_archive", "read_local_memory", "edit_fixture", "run_fixture_checks"]
COMMON_PROMPT = (
    "Continue the deployment policy task for workstream {stream}. Update policy.json "
    "with the accepted backend, rejected backend, and next step as the keys "
    "backend, rejected, next_step. Research the available history under archive/ "
    "and local-memory/ as needed. Apply the current decision to this workstream."
)


def write_json(path: Path, value):
    with path.open("x", encoding="utf-8") as out:
        json.dump(value, out, ensure_ascii=False, indent=2, allow_nan=False)
        out.write("\n")


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, ensure_ascii=False).encode()).hexdigest()


def cases(split):
    """Six development templates and a 30-case smoke matrix, never mixed.

    The evaluation-shaped matrix repeats these templates with different IDs.
    It is not a held-out corpus of 30 independent developer tasks.
    """
    result = []
    for category_index, category in enumerate(CATEGORIES):
        for variant in range(1 if split == "development" else 5):
            identity = f"{'D' if split == 'development' else 'E'}{category_index * (1 if split == 'development' else 5) + variant + 1:02d}"
            stream = f"deployment-{identity.lower()}"
            accepted = f"sqlite-{identity.lower()}"
            rejected = f"redis-{identity.lower()}"
            next_step = f"verify-restore-{identity.lower()}"
            direction = "claude-to-codex" if (category_index + variant) % 2 == 0 else "codex-to-claude"
            writer, reader = ("claude", "codex") if direction == "claude-to-codex" else ("codex", "claude")
            archive = []

            def event(text, *, workstream=stream, speaker="user"):
                minute = len(archive)
                at = (dt.datetime(2026, 9, 1, tzinfo=dt.timezone.utc) + dt.timedelta(minutes=minute)).isoformat()
                archive.append(dict(id=f"{identity}-{minute:04d}", at=at, agent=writer,
                                    speaker=speaker, workstream=workstream, text=text))

            if category == "superseded":
                event(f"Earlier rule: use {rejected}; this was the old policy.")
            elif category == "failed-attempt":
                event(f"We tried {rejected}. Restore lost acknowledged writes; this experiment failed.", speaker="assistant")
            elif category == "rejected":
                event(f"Proposal: use {rejected}. It has not been approved.", speaker="assistant")
            event(f"Decision for {stream}: use {accepted}. Reject {rejected}. Next step is {next_step}."
                  + (" This supersedes the earlier rule." if category == "superseded" else ""))
            if category == "long-middle":
                # The decision is in the middle, with substantial unrelated
                # history before and after it. Only the fixture, not a real trace.
                decision = archive.pop()
                for number in range(80):
                    event(f"Unrelated build note {number}: " + "compiler cache statistics and formatting review. " * 12)
                event(decision["text"])
                for number in range(80):
                    event(f"Unrelated lint note {number}: " + "documentation links and build timings. " * 12)
            if category == "parallel":
                event(f"For deployment-other use {rejected}; reject {accepted}; next step is enable-cache.",
                      workstream="deployment-other")
            event("The decision is accepted. Continue with the next step when another agent takes over.", speaker="assistant")
            result.append(dict(id=identity, split=split, category=category, direction=direction,
                               writer=writer, reader=reader, workstream=stream, archive=archive,
                               task_at="2026-09-03T00:00:00+00:00",
                               expected=dict(backend=accepted, rejected=rejected, next_step=next_step)))
    return result


def validate_cases(items):
    ids = [case["id"] for case in items]
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate scenario IDs")
    for case in items:
        cutoff = dt.datetime.fromisoformat(case["task_at"])
        if cutoff.tzinfo is None:
            raise ValueError("task cutoff requires a timezone")
        for event in case["archive"]:
            at = dt.datetime.fromisoformat(event["at"])
            if at.tzinfo is None or at >= cutoff:
                raise ValueError(f"{case['id']}: archive includes an event at or after the task cutoff")


def prepare(root: Path, claude_model: str, codex_model: str, *, split="evaluation", local_memory=None,
            budget_path=None, corpus_path=None):
    items, corpus_info = import_corpus(corpus_path, split) if corpus_path else (cases(split), None)
    validate_cases(items)
    if root.exists():
        raise ValueError("run root exists; preparation never overwrites a run")
    memory = {}
    if local_memory:
        # An explicit, text-only snapshot; no home-directory config or plugin
        # discovery. Every trial receives the exact same snapshot.
        for path in sorted(local_memory.rglob("*.md")):
            if path.is_symlink():
                raise ValueError("local memory snapshot may not contain symlinks")
            memory[path.relative_to(local_memory).as_posix()] = path.read_text(encoding="utf-8")
    root.mkdir(parents=True)
    # Development, evaluation and replacement runs share the experiment cap.
    # The CLI requires an explicit ledger; callers use a shared sibling default.
    budget_path = (budget_path or root.parent / "continuity-gemini-budget.sqlite").resolve()
    budget_path.parent.mkdir(parents=True, exist_ok=True)
    models = dict(claude=claude_model, codex=codex_model)
    trials = []
    permutations = list(itertools.permutations(METHODS))
    for index, case in enumerate(items):
        for repeat in range(REPEATS):
            for method in permutations[(index + repeat) % len(permutations)]:
                identity = f"{case['id']}-{repeat + 1}-{method}"
                repo = root / "trials" / identity / "repo"
                (repo / "archive").mkdir(parents=True)
                (repo / "local-memory").mkdir()
                for name, text in memory.items():
                    target = repo / "local-memory" / name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(text, encoding="utf-8")
                (repo / "archive" / "history.jsonl").write_text(
                    "".join(json.dumps(e, ensure_ascii=False) + "\n" for e in case["archive"]), encoding="utf-8")
                for name, text in case.get("seed", {"policy.json": "{}\n"}).items():
                    target = repo / name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_text(text, encoding="utf-8")
                trial = dict(id=identity, scenario=case["id"], repeat=repeat + 1, method=method,
                             reader=case["reader"], model=models[case["reader"]], direction=case["direction"],
                             archive_sha256=fingerprint(case["archive"]), memory_sha256=fingerprint(memory),
                             prompt=case.get("prompt", COMMON_PROMPT.format(stream=case["workstream"])),
                             permissions=PERMISSIONS, budget=dict(seconds=900, actions=40),
                             profile=f"trials/{identity}/profile", repo=f"trials/{identity}/repo")
                # Profiles and Anamnesis stores belong to one trial, not one
                # method or one repeated case. No hooks/trust are fabricated.
                for name in ("claude", "codex", "anamnesis-data"):
                    (root / trial["profile"] / name).mkdir(parents=True)
                write_json(root / "trials" / identity / "trial.json", trial)
                trials.append(trial)
    manifest = dict(format=1, split=split, corpus="externally-supplied-v1" if corpus_info else "controlled-fixture-v1", repeats=REPEATS,
                    scenarios=items, trials=trials, models=models,
                    pricing=dict(model="gemini-3.6-flash", input_usd_per_million="0.75",
                                 output_usd_per_million="3.75", includes_thinking=True,
                                 service_tier="standard", verified_on="2026-10-03",
                                 expires_on="2026-12-31", access_verified=False,
                                 source="https://ai.google.dev/gemini-api/docs/pricing"),
                    gemini_budget_usd="50.00", budget_ledger=str(budget_path), claude_paused=True,
                    evidence_status="prepared; no agent task or API call has run")
    if corpus_info:
        manifest["corpus_review"] = corpus_info
    manifest["fingerprint"] = fingerprint(manifest)
    write_json(root / "manifest.json", manifest)
    ledger = Budget(budget_path, manifest["pricing"])
    ledger.close()
    return manifest


def load(root):
    manifest = json.loads((root / "manifest.json").read_text(encoding="utf-8"))
    signed = dict(manifest)
    claimed = signed.pop("fingerprint")
    if fingerprint(signed) != claimed:
        raise ValueError("frozen manifest changed; create a new run instead")
    return manifest


def record(root, trial_id, evidence):
    manifest = load(root)
    trial = next((t for t in manifest["trials"] if t["id"] == trial_id), None)
    if trial is None:
        raise ValueError("unknown trial")
    if evidence.get("reader") != trial["reader"] or evidence.get("model") != trial["model"]:
        raise ValueError("reader/model differs from the frozen trial; Gemini cannot replace a reader")
    status = evidence.get("status")
    if status not in ("complete", "quota", "budget", "error", "paused"):
        raise ValueError("invalid trial status")
    for name in ("elapsed_seconds", "preparation_seconds", "tool_calls", "user_reminders"):
        value = evidence.get(name)
        if type(value) not in (int, float) or not math.isfinite(value) or value < 0:
            raise ValueError(f"{name} must be a finite nonnegative measurement")
    for name in ("tokens", "preparation_tokens", "agent_cost_usd"):
        value = evidence.get(name)
        if value is not None and (type(value) not in (int, float) or not math.isfinite(value) or value < 0):
            raise ValueError(f"{name} must be nonnegative or null when unavailable")
    trace_hashes = {}
    trial_root = (root / "trials" / trial_id).resolve()
    for stage in ("captured", "kept", "shown", "source_opened"):
        value = evidence.get(stage)
        if value is not None and type(value) is not bool:
            raise ValueError(f"{stage} must be bool or null")
        if value is True and not evidence.get(f"{stage}_evidence"):
            raise ValueError(f"{stage} requires a trace reference")
        if value is True:
            reference = evidence[f"{stage}_evidence"]
            if not isinstance(reference, str) or Path(reference).is_absolute():
                raise ValueError("trace reference must be a relative path within the trial")
            trace = (trial_root / reference).resolve()
            if not trace.is_relative_to(trial_root) or not trace.is_file():
                raise ValueError("trace reference must name an existing file within the trial")
            trace_hashes[stage] = hashlib.sha256(trace.read_bytes()).hexdigest()
    repo = root / trial["repo"]
    archived = [json.loads(line) for line in (repo / "archive/history.jsonl").read_text(encoding="utf-8").splitlines()]
    if fingerprint(archived) != trial["archive_sha256"]:
        raise ValueError("history changed during the task")
    memory = {p.relative_to(repo / "local-memory").as_posix(): p.read_text(encoding="utf-8")
              for p in sorted((repo / "local-memory").rglob("*.md"))}
    if fingerprint(memory) != trial["memory_sha256"]:
        raise ValueError("local memory snapshot changed during the task")
    case = next(c for c in manifest["scenarios"] if c["id"] == trial["scenario"])
    expected = case.get("expected", {})
    try:
        actual = json.loads((repo / "policy.json").read_text(encoding="utf-8"))
    except (ValueError, OSError):
        actual = None
    # Completion alone never means success: check the produced artifact.
    passed = status == "complete" and isinstance(actual, dict) and all(
        actual.get(key) == value for key, value in expected.items())
    result = dict(evidence, trial=trial_id, passed=passed, artifact=actual,
                  trace_sha256=trace_hashes,
                  decision_correct=status == "complete" and isinstance(actual, dict)
                  and actual.get("backend") == expected.get("backend"),
                  wrong_decision=status == "complete" and isinstance(actual, dict)
                  and actual.get("backend") == expected.get("rejected"))
    if "checks" in case:
        checked = [check_artifact(repo, check) for check in case["checks"]]
        artifacts = {check["file"]: hashlib.sha256((repo / check["file"]).read_bytes()).hexdigest()
                     for check in case["checks"] if (repo / check["file"]).resolve().is_relative_to(repo.resolve())
                     and (repo / check["file"]).is_file()}
        result.update(passed=status == "complete" and all(checked), artifact_checks=checked,
                      artifact_sha256=artifacts,
                      decision_correct=status == "complete" and check_artifact(repo, case["decision_check"]),
                      wrong_decision=status == "complete" and check_artifact(repo, case["stale_decision_check"]))
    write_json(root / "trials" / trial_id / "result.json", result)
    return result


def cluster_interval(values, samples=5000):
    """Resample scenarios, keeping all repeats of a scenario together."""
    rng = random.Random(47)
    averages = sorted(statistics.mean(rng.choices(values, k=len(values))) for _ in range(samples))
    return [averages[int(samples * .025)], averages[min(samples - 1, int(samples * .975))]]


def report(root):
    manifest = load(root)
    results = {}
    for trial in manifest["trials"]:
        path = root / "trials" / trial["id"] / "result.json"
        if path.exists():
            results[trial["id"]] = json.loads(path.read_text(encoding="utf-8"))
    arms = {}
    by_case = {}
    for method in METHODS:
        planned = [t for t in manifest["trials"] if t["method"] == method]
        completed = [results[t["id"]] for t in planned
                     if t["id"] in results and results[t["id"]]["status"] == "complete"]
        attempted = [results[t["id"]] for t in planned if t["id"] in results]
        passed = sum(r["passed"] for r in completed)
        missing = len(planned) - len(completed)
        arms[method] = dict(planned=len(planned), completed=len(completed), incomplete=missing,
                            passed=passed, success_bounds=[passed / len(planned), (passed + missing) / len(planned)],
                            statuses=dict(Counter(results[t["id"]]["status"] if t["id"] in results else "pending" for t in planned)),
                            wrong_decisions=sum(r["wrong_decision"] for r in completed),
                            correct_decisions=sum(r["decision_correct"] for r in completed),
                            user_reminders=sum(r["user_reminders"] for r in attempted),
                            measured_seconds=sum(r["elapsed_seconds"] + r["preparation_seconds"] for r in attempted),
                            tool_calls=sum(r["tool_calls"] for r in attempted),
                            agent_cost_known_usd=sum(r.get("agent_cost_usd") or 0 for r in attempted),
                            agent_cost_unknown=sum(r.get("agent_cost_usd") is None for r in attempted),
                            stages={stage: dict(Counter("unknown" if r.get(stage) is None else str(r[stage]).lower() for r in attempted))
                                    for stage in ("captured", "kept", "shown", "source_opened", "decision_correct")})
        by_case[method] = {}
        for case in manifest["scenarios"]:
            rows = [results[t["id"]] for t in planned if t["scenario"] == case["id"] and t["id"] in results]
            if len(rows) == REPEATS and all(r["status"] == "complete" for r in rows):
                tokens = [None if r.get("tokens") is None or r.get("preparation_tokens") is None
                          else r["tokens"] + r["preparation_tokens"] for r in rows]
                by_case[method][case["id"]] = dict(success=statistics.mean(r["passed"] for r in rows),
                    seconds=statistics.mean(r["elapsed_seconds"] + r["preparation_seconds"] for r in rows),
                    tokens=None if None in tokens else statistics.mean(tokens))
    output = dict(corpus=manifest["corpus"], split=manifest["split"], arms=arms,
                  comparisons={}, gate="incomplete; no usefulness claim", planned=len(manifest["trials"]))
    ledger = Budget(Path(manifest["budget_ledger"]), manifest["pricing"])
    try:
        output["gemini_cost"] = ledger.report()
    finally:
        ledger.close()
    if any(arm["incomplete"] for arm in arms.values()):
        return output
    # Success is paired at the scenario level, never 150 independent cases.
    keys = [case["id"] for case in manifest["scenarios"]]
    strongest = max(("history", "brief"), key=lambda m: (arms[m]["passed"], -arms[m]["measured_seconds"]))
    for method in ("history", "brief"):
        differences = [by_case["anamnesis"][k]["success"] - by_case[method][k]["success"] for k in keys]
        interval = cluster_interval(differences)
        metrics = dict(success_delta=statistics.mean(differences), success_delta_ci95=interval)
        for resource in ("seconds", "tokens"):
            pairs = [(by_case["anamnesis"][k][resource], by_case[method][k][resource]) for k in keys]
            if any(a is None or b is None or b <= 0 for a, b in pairs):
                metrics[f"{resource}_ratio_ci95"] = None
            else:
                metrics[f"{resource}_ratio_ci95"] = cluster_interval([a / b for a, b in pairs])
        output["comparisons"][method] = metrics
    candidate = output["comparisons"][strongest]
    accuracy = candidate["success_delta_ci95"][0] >= .10
    similar = candidate["success_delta_ci95"][0] >= -.02
    efficient = any(candidate[f"{resource}_ratio_ci95"] is not None and candidate[f"{resource}_ratio_ci95"][1] <= .80
                    for resource in ("seconds", "tokens"))
    output["strongest_alternative"] = strongest
    output["gate"] = "met" if accuracy or (similar and efficient) else "not met or uncertain"
    if manifest["split"] != "evaluation":
        output["gate"] = "development only; not an evaluation gate"
    elif manifest["corpus"] == "controlled-fixture-v1":
        output["gate"] = "controlled fixtures only; not held-out developer-task evidence"
    else:
        output["gate"] = "external corpus and client isolation require independent acceptance review"
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    prep = sub.add_parser("prepare")
    prep.add_argument("--root", type=Path, required=True)
    prep.add_argument("--claude-model", required=True)
    prep.add_argument("--codex-model", required=True)
    prep.add_argument("--split", choices=("development", "evaluation"), default="evaluation")
    prep.add_argument("--local-memory", type=Path)
    prep.add_argument("--corpus", type=Path, help="private frozen corpus: six development and thirty distinct evaluation tasks")
    prep.add_argument("--budget-ledger", type=Path, required=True,
                      help="same shared ledger for development, evaluation, retries and replacement runs")
    add = sub.add_parser("record")
    add.add_argument("--root", type=Path, required=True)
    add.add_argument("--trial", required=True)
    add.add_argument("--evidence", type=Path, required=True)
    show = sub.add_parser("report")
    show.add_argument("--root", type=Path, required=True)
    hold = sub.add_parser("reserve")
    hold.add_argument("--root", type=Path, required=True)
    hold.add_argument("--model", required=True)
    hold.add_argument("--max-input", type=int, required=True)
    hold.add_argument("--max-output", type=int, required=True)
    hold.add_argument("--purpose", choices=("access-check", "preparation", "summary", "brief", "retry"), required=True)
    settle = sub.add_parser("settle")
    settle.add_argument("--root", type=Path, required=True)
    settle.add_argument("--reservation", required=True)
    settle.add_argument("--model", required=True)
    settle.add_argument("--input", type=int, required=True)
    settle.add_argument("--output", type=int, required=True, help="including thinking tokens")
    args = parser.parse_args()
    if args.command == "prepare":
        manifest = prepare(args.root, args.claude_model, args.codex_model,
                           split=args.split, local_memory=args.local_memory, budget_path=args.budget_ledger,
                           corpus_path=args.corpus)
        print(f"Prepared {len(manifest['scenarios'])} scenarios, {len(manifest['trials'])} trials; no agents started.")
    elif args.command == "record":
        print(json.dumps(record(args.root, args.trial, json.loads(args.evidence.read_text(encoding="utf-8")))))
    elif args.command == "report":
        print(json.dumps(report(args.root), ensure_ascii=False, indent=2, allow_nan=False))
    else:
        manifest = load(args.root)
        ledger = Budget(Path(manifest["budget_ledger"]), manifest["pricing"])
        try:
            if args.command == "reserve":
                print(ledger.reserve(args.model, args.max_input, args.max_output, args.purpose))
            else:
                ledger.settle(args.reservation, args.model, args.input, args.output)
                print(json.dumps(ledger.report()))
        finally:
            ledger.close()


if __name__ == "__main__":
    main()
