#!/usr/bin/env python3
"""Does memory change what a real agent does, sessions later?

`anamnesis eval` measures whether memory finds the page that answers a
question. This measures the thing that finding is for: an agent told something
in one session, given unrelated work, and then handed a task that needs what it
was told. The scenario in scenario.toml runs twice, on two copies of the same
small repository, session by session:

  memory   hooks and the MCP tools wired to a server of its own, in a data
           directory of its own, with the model and embedder this machine's
           server uses
  control  the same prompts, the same model, the same tools, and nothing that
           carries anything from one session to the next

Each session is a headless `claude -p`. After each one the repository it left is
judged by a check in checks.py that runs the code rather than asking a model.

    python longrun.py selftest            the checks, against right and wrong answers
    python longrun.py run --anamnesis PATH one repeat of the scenario, both arms
    python longrun.py report              every run so far, side by side

A repeat takes one to two hours and asks the consolidation model about twelve
sessions, which is why it is meant to run once a night rather than all at once.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import shutil
import subprocess
import sys
import time
import tomllib
import urllib.error
import urllib.request
import uuid
from collections import Counter
from pathlib import Path

import checks

HERE = Path(__file__).resolve().parent
SCENARIO = HERE / "scenario.toml"
FIXTURE = checks.FIXTURE

DEFAULT_MODEL = "claude-haiku-4-5"
DEFAULT_PORT = 18080
MAX_TURNS = 40
SESSION_TIMEOUT = 45 * 60

# What the agent may do without being asked. The same list for both arms; the
# memory arm's MCP server adds its own tools under the one name that allows them.
# A tool left off is refused rather than prompted for, since nobody is there to
# answer, and the refusals are counted per session.
#
# Three things are left off on purpose, and stay off: `git add` and `git
# commit`, because the harness commits each session itself and a session that
# commits its own work would write a history the harness then writes again;
# deleting, because an unattended nightly run should not hold an unbounded
# `rm`; and installing, because a run that reaches the network for a package
# measures that machine's network. Each of those refusals costs a turn, which
# is why the count is reported rather than left to be read off a transcript.
ALLOWED_TOOLS = [
    "Read",
    "Edit",
    "Write",
    "Glob",
    "Grep",
    "LS",
    "TodoWrite",
    "Bash(python:*)",
    "Bash(python3:*)",
    "Bash(py:*)",
    "Bash(cd:*)",
    "Bash(ls:*)",
    "Bash(cat:*)",
    "Bash(head:*)",
    "Bash(find:*)",
    "Bash(grep:*)",
    "Bash(git status:*)",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    # The fixture's tests read LEDGER_FIXTURES from the environment, so the
    # natural way to run one — `LEDGER_FIXTURES=tests/fixtures python -m
    # unittest ...` — does not begin with `python` and was refused four times
    # in the first complete run, while `python tools/check.py`, which sets the
    # variable itself, was allowed. The scenario plants nothing about how the
    # tests are run, so each of those refusals measured this list and not
    # memory.
    "Bash(env:*)",
    "Bash(export:*)",
    "Bash(LEDGER_FIXTURES=*)",
    "mcp__anamnesis",
]

# Tools the agent is not given at all, rather than refused when it reaches for
# one. On Windows a session has a PowerShell tool beside Bash, and the first
# complete run spent 31 calls on it, 27 of them refused, because it was not on
# the list above. No rule narrows it: with only `PowerShell(python:*)` allowed,
# `Get-ChildItem` ran — measured here on 2026-09-17 — so naming that tool at
# all hands an unattended nightly run an unbounded shell, and the alternative
# is to take it away. `--disallowedTools` removes it from the session rather
# than refusing it in the moment: asked, an agent started this way answers
# that it has no PowerShell tool, and reaches for the shell both arms share.
DISALLOWED_TOOLS = ["PowerShell"]

ISOLATION_PROMPT = (
    "Answer with one word, YES or NO. Do your instructions or your tools give you "
    "any persistent memory that carries over between sessions, such as a memory "
    "directory, a MEMORY.md file, or memory tools?"
)

COUNTED_FOOTER = "Compiled without a model"


# ---------------------------------------------------------------------------
# Scenario


def load_scenario(path: Path = SCENARIO) -> dict:
    scenario = tomllib.loads(path.read_text(encoding="utf-8"))
    sessions = scenario["session"]
    ids = [session["id"] for session in sessions]
    problems = []
    if len(ids) != len(set(ids)):
        problems.append("session ids repeat")
    for session in sessions:
        if session["check"] not in checks.CHECKS:
            problems.append(f"{session['id']} names an unknown check {session['check']!r}")
        for field in ("plants", "needs"):
            if field in session and session[field] not in ids:
                problems.append(f"{session['id']} {field} {session[field]!r}, which is not a session")
        if session["kind"] not in ("plant", "distractor", "probe"):
            problems.append(f"{session['id']} has kind {session['kind']!r}")
        session["prompt"] = " ".join(session["prompt"].split())
    if problems:
        raise SystemExit("scenario.toml: " + "; ".join(problems))
    return scenario


# ---------------------------------------------------------------------------
# Places


def default_root() -> Path:
    if os.name == "nt":
        base = Path(os.environ.get("LOCALAPPDATA", Path.home()))
    elif sys.platform == "darwin":
        base = Path.home() / "Library" / "Application Support"
    else:
        base = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share"))
    return base / "anamnesis-longrun"


def live_data_dir() -> Path:
    """Where this machine's anamnesis keeps its data, for its settings.env."""
    if os.environ.get("ANAMNESIS_DATA_DIR"):
        return Path(os.environ["ANAMNESIS_DATA_DIR"])
    if os.name == "nt":
        return Path(os.environ["APPDATA"]) / "anamnesis"
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Application Support" / "anamnesis"
    return Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local" / "share")) / "anamnesis"


# ---------------------------------------------------------------------------
# Processes


def git(repo: Path, *args: str) -> str:
    result = subprocess.run(["git", *args], cwd=repo, capture_output=True, text=True, encoding="utf-8", errors="replace", check=True)
    return result.stdout


def prepare_repo(repo: Path, project: str | None) -> None:
    shutil.copytree(FIXTURE, repo, ignore=shutil.ignore_patterns("__pycache__"))
    git(repo, "init", "-q")
    git(repo, "config", "user.name", "longrun")
    git(repo, "config", "user.email", "longrun@example.invalid")
    git(repo, "config", "core.autocrlf", "false")
    if project:
        (repo / ".anamnesis.toml").write_text(
            f'[scope]\nworkspace = "longrun"\nproject = "{project}"\n', encoding="utf-8"
        )
    # What wiring writes into the checkout is the harness's, not the agent's
    # work, and stays out of the commits a session is judged from.
    exclude = repo / ".git" / "info" / "exclude"
    exclude.parent.mkdir(parents=True, exist_ok=True)
    exclude.write_text(".claude/\n.mcp.json\n.anamnesis.toml\n__pycache__/\n", encoding="utf-8")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "fixture")


def commit_session(repo: Path, session_id: str) -> str:
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "--allow-empty", "-m", session_id)
    return git(repo, "show", "--stat", "--format=", "HEAD").strip()


class Server:
    """An anamnesis server for the memory arm, on its own port and data."""

    def __init__(self, binary: Path, data: Path, port: int, log: Path):
        self.binary, self.data, self.port, self.log = binary, data, port, log
        self.process: subprocess.Popen | None = None

    def env(self) -> dict:
        env = dict(os.environ, ANAMNESIS_DATA_DIR=str(self.data))
        # No settings.env copied means no model, said explicitly: a key in the
        # account's credential store is read by every data directory.
        if not (self.data / "settings.env").exists():
            env["ANAMNESIS_LLM_PROVIDER"] = "none"
        return env

    def start(self) -> None:
        if self.healthy():
            raise SystemExit(f"something already answers on port {self.port}; pass --port")
        handle = open(self.log, "ab")
        self.process = subprocess.Popen(
            [str(self.binary), "serve", "--port", str(self.port), "--no-watch"],
            env=self.env(),
            stdout=handle,
            stderr=subprocess.STDOUT,
        )
        for _ in range(120):
            if self.healthy():
                return
            if self.process.poll() is not None:
                raise SystemExit(f"the server exited with {self.process.returncode}; see {self.log}")
            time.sleep(0.5)
        raise SystemExit(f"the server did not answer on port {self.port}; see {self.log}")

    def healthy(self) -> bool:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{self.port}/health", timeout=2) as response:
                return response.status == 200
        except (urllib.error.URLError, OSError):
            return False

    def stop(self) -> None:
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=30)
            except subprocess.TimeoutExpired:
                self.process.kill()


def claude_args(claude: str, model: str, max_turns: int, mcp_config: Path | None) -> list[str]:
    args = [
        claude,
        "-p",
        "--model",
        model,
        "--output-format",
        "stream-json",
        "--verbose",
        "--max-turns",
        str(max_turns),
        # Project and local settings only: the memory arm's hooks live in the
        # checkout, and whatever this machine's user settings wire up belongs
        # to neither arm.
        "--setting-sources",
        "project,local",
        "--strict-mcp-config",
        "--allowedTools",
        ",".join(ALLOWED_TOOLS),
        "--disallowedTools",
        ",".join(DISALLOWED_TOOLS),
    ]
    if mcp_config:
        args += ["--mcp-config", str(mcp_config)]
    return args


def claude_env() -> dict:
    env = dict(os.environ)
    env["CLAUDE_CODE_DISABLE_AUTO_MEMORY"] = "1"
    env.pop("ANAMNESIS_DATA_DIR", None)
    return env


def summarize_stream(stream: str) -> dict:
    """What one `stream-json` transcript says the session did."""
    tools: Counter = Counter()
    tool_errors = 0
    init: dict = {}
    result: dict = {}
    for line in stream.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = event.get("type")
        if kind == "system" and event.get("subtype") == "init":
            init = event
        elif kind == "assistant":
            for block in event.get("message", {}).get("content", []) or []:
                if isinstance(block, dict) and block.get("type") == "tool_use":
                    tools[block.get("name", "?")] += 1
        elif kind == "user":
            for block in event.get("message", {}).get("content", []) or []:
                if isinstance(block, dict) and block.get("type") == "tool_result" and block.get("is_error"):
                    tool_errors += 1
        elif kind == "result":
            result = event
    mcp = {server.get("name"): server.get("status") for server in init.get("mcp_servers", []) or []}
    denials = Counter(denial.get("tool_name", "?") for denial in result.get("permission_denials") or [])
    usage = result.get("usage", {}) or {}
    return {
        "claude_session": init.get("session_id") or result.get("session_id"),
        "mcp_servers": mcp,
        "turns": result.get("num_turns"),
        "cost_usd": result.get("total_cost_usd"),
        "duration_s": round((result.get("duration_ms") or 0) / 1000, 1),
        "is_error": result.get("is_error"),
        "stop": result.get("subtype") or result.get("terminal_reason"),
        "input_tokens": (usage.get("input_tokens") or 0)
        + (usage.get("cache_read_input_tokens") or 0)
        + (usage.get("cache_creation_input_tokens") or 0),
        "output_tokens": usage.get("output_tokens"),
        "permission_denials": sum(denials.values()),
        "denials_by_tool": dict(denials),
        "tools": dict(tools),
        "tool_errors": tool_errors,
        "memory_calls": sum(count for name, count in tools.items() if name.startswith("mcp__anamnesis__")),
        "answer": (result.get("result") or "")[-600:],
    }


def run_claude(claude: str, repo: Path, prompt: str, model: str, max_turns: int, mcp_config: Path | None, log: Path) -> dict:
    started = time.time()
    try:
        proc = subprocess.run(
            claude_args(claude, model, max_turns, mcp_config),
            cwd=repo,
            input=prompt,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            env=claude_env(),
            timeout=SESSION_TIMEOUT,
        )
        stdout, stderr, code = proc.stdout, proc.stderr, proc.returncode
    except subprocess.TimeoutExpired as expired:
        stdout = expired.stdout.decode("utf-8", "replace") if isinstance(expired.stdout, bytes) else (expired.stdout or "")
        stderr, code = "timed out", None
    log.write_text(stdout, encoding="utf-8")
    log.with_suffix(".stderr.txt").write_text(stderr, encoding="utf-8")
    summary = summarize_stream(stdout)
    summary["exit_code"] = code
    summary["wall_s"] = round(time.time() - started, 1)
    return summary


def recorded_session(data: Path, project: str, claude_session: str | None) -> str | None:
    """anamnesis's id for the session Claude Code called `claude_session`.

    Derived the way the server derives it, a v5 of the harness's id under the
    project's, with the project's id read from the transcript the server
    spooled rather than derived a second time here.
    """
    if not claude_session:
        return None
    for spool in sorted((data / "raw" / "longrun" / project).glob("*/*.jsonl")):
        try:
            header = json.loads(spool.read_text(encoding="utf-8").splitlines()[0])
        except (IndexError, json.JSONDecodeError, OSError):
            continue
        if header.get("project_id"):
            return str(uuid.uuid5(uuid.UUID(header["project_id"]), f"session:{claude_session}"))
    return None


def consolidation_model(settings: Path) -> str | None:
    """`provider:model` from a settings.env, for the record.

    Two runs whose memory arms wrote their pages with different models are two
    experiments, and `report` pools runs. Naming it per run is what keeps a
    local model's repeat from being read as part of a hosted model's.
    """
    provider = model = None
    try:
        for line in settings.read_text(encoding="utf-8", errors="replace").splitlines():
            name, _, value = line.partition("=")
            if name.strip() == "ANAMNESIS_LLM_PROVIDER":
                provider = value.strip()
            elif name.strip() == "ANAMNESIS_LLM_MODEL":
                model = value.strip()
    except OSError:
        return None
    return ":".join(part for part in (provider, model) if part) or None


def log_size(log: Path) -> int:
    return log.stat().st_size if log.exists() else 0


def model_gave_up(log: Path, since: int) -> bool:
    """Whether the server has logged, since byte `since`, that a model did not
    write a page. Written once per session, after the whole fallback chain."""
    if not log.exists():
        return False
    with open(log, "rb") as handle:
        handle.seek(since)
        return b"using the counted summary" in handle.read()


def wait_for_page(
    data: Path,
    project: str,
    claude_session: str | None,
    server_log: Path,
    log_from: int,
    appear: int = 180,
    enrich: int = 600,
) -> dict:
    """The page a session left, once a model has written it or given up.

    The counted page is written the moment the session ends and the model's
    replaces it after. The wait ends when the page stops being counted, when
    the server logs that the model did not answer, or when `enrich` runs out;
    a page still counted then is recorded as counted, which makes every probe
    after it in the memory arm suspect.
    """
    deadline = time.time() + appear
    page = None
    session = None
    while time.time() < deadline:
        session = session or recorded_session(data, project, claude_session)
        if session:
            found = sorted((data / "wiki" / "longrun" / project / "sessions").glob(f"*-{session[:8]}.md"))
            if found:
                page = found[-1]
                break
        time.sleep(3)
    if page is None:
        return {"page": None, "source": "none", "session": session}
    deadline = time.time() + enrich
    while time.time() < deadline:
        text = page.read_text(encoding="utf-8", errors="replace")
        if COUNTED_FOOTER not in text:
            return {"page": page.name, "source": "model", "session": session}
        if model_gave_up(server_log, log_from):
            return {"page": page.name, "source": "counted", "session": session}
        time.sleep(5)
    return {"page": page.name, "source": "counted", "session": session}


def nothing_left_to_measure(session: dict, arm: str, record: dict) -> str | None:
    """Why a run should stop here, or None to go on.

    `report` excludes every probe whose planting session's page was not
    written by a model, so a planting page that comes back counted has already
    cost the run every probe behind it. A model that refused one session
    refuses the rest of the hour: on 2026-09-17 a run started against a spent
    free-tier quota wrote its first page by counting and would have spent two
    hours and $1.59 on the eleven sessions after it, measuring nothing. The
    check before the run asks one small question, which a model out of quota
    can still answer; this asks the same question of the work.
    """
    if arm != "memory" or session["kind"] != "plant":
        return None
    page = record["page"] or {}
    if page.get("source") == "model":
        return None
    what = "no page at all" if page.get("source") == "none" else "a page written by counting"
    return (
        f"{session['id']} plants what {session.get('plants', 'a later probe')} needs and left {what}, "
        "so that probe would be excluded and the model is unlikely to answer for the ones after it"
    )


def isolation_check(claude: str, model: str, scratch: Path) -> dict:
    """Ask the control arm's setup whether it carries memory, before trusting it.

    Claude Code has a memory of its own, and a control arm that quietly had
    one would measure nothing. The answer is a model's and not proof, but a
    YES is enough to stop.
    """
    scratch.mkdir(parents=True, exist_ok=True)
    summary = run_claude(claude, scratch, ISOLATION_PROMPT, model, 2, None, scratch / "isolation.jsonl")
    answer = summary["answer"].strip().upper()
    return {"answer": summary["answer"].strip(), "isolated": answer.startswith("NO"), "cost_usd": summary["cost_usd"]}


# ---------------------------------------------------------------------------
# Run


def write_json(path: Path, value) -> None:
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False), encoding="utf-8")
    temporary.replace(path)


def version_of(command: list[str]) -> str:
    try:
        return subprocess.run(command, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60).stdout.strip()
    except (OSError, subprocess.TimeoutExpired) as error:
        return f"unavailable: {error}"


def redirection_reason(asked: Path, real: Path) -> str | None:
    """Whether what was written at `asked` really landed somewhere else.

    A Microsoft Store Python runs inside its package's filesystem
    redirection: everything it writes under %LOCALAPPDATA% lands in that
    package's LocalCache instead. Nothing tells it so — `exists()` is true,
    reads come back — and the anamnesis binary, which is not in the package,
    reads the path it was handed and finds it empty.

    On 2026-09-17 that stopped a run at the model check with `no model is
    configured`, about a settings.env this harness had copied a second
    earlier. Every later path would have been wrong the same way: the memory
    arm's data directory, its server log, both repositories.
    """
    if os.path.normcase(str(asked)) == os.path.normcase(str(real)):
        return None
    # ASCII, for the same reason the refusals above are read without their
    # mark: this is printed on the console that stopped the run, and this
    # machine's is cp1254.
    return (
        f"this Python writes {asked} to {real} instead, and the anamnesis binary "
        f"reads the first, so its data directory, its server log and both "
        f"repositories would be written where nothing else can see them. It is a "
        f"Microsoft Store Python if `sys.base_prefix` is under WindowsApps; use one "
        f"that is not, or pass --root a directory outside %LOCALAPPDATA%"
    )


def redirected(run_dir: Path) -> str | None:
    """`redirection_reason` for a directory this run is about to fill.

    Windows only, and not because the other platforms are trusted: there a
    resolved path that differs from the asked one is ordinarily a symlink,
    which is not this failure. `/tmp` is `/private/tmp` on macOS, and a run
    rooted there is fine — another process handed the first path finds the
    file. A package redirection is the case where it does not.
    """
    if os.name != "nt":
        return None
    probe = run_dir / ".probe"
    probe.write_text("probe", encoding="utf-8")
    try:
        return redirection_reason(probe, Path(os.path.realpath(probe)))
    finally:
        probe.unlink(missing_ok=True)


def model_check_verdict(returncode: int, output: str) -> tuple[bool, str]:
    """Read `anamnesis key check`: whether the memory arm's model can be shown
    to work, and one line saying why not.

    A binary from before the command answers with clap's usage error, which is
    not a verdict about the key, and is named as what it is.
    """
    if returncode == 0:
        return True, "every model answered"
    if "unrecognized subcommand" in output or "unexpected argument" in output:
        return False, "this anamnesis has no `key check`; use a newer build or pass --skip-model-check"
    # Without its mark: the reason is printed, and on a Windows console in a
    # legacy code page (cp1254 on the machine this runs on) printing "❌"
    # raised UnicodeEncodeError — at the one moment the run had to say why it
    # stopped.
    refusals = [line.strip() for line in output.splitlines() if line.strip().startswith(("❌", "⚠"))]
    if refusals:
        return False, refusals[0].lstrip("❌⚠️").strip()
    last = [line.strip() for line in output.splitlines() if line.strip()]
    return False, last[-1] if last else f"key check exited {returncode} and said nothing"


def model_check(binary: Path, settings: Path, scratch: Path) -> dict:
    """Ask the memory arm's model, with the settings its server will read,
    before anything is spent on sessions.

    The report excludes every probe whose planting session's page was counted,
    so a run under a key that is refused costs two hours and a few dollars and
    measures nothing. On 2026-09-14 that was the state of this machine's key.
    """
    data = scratch / "data"
    data.mkdir(parents=True)
    shutil.copy2(settings, data / "settings.env")
    env = dict(os.environ, ANAMNESIS_DATA_DIR=str(data))
    try:
        proc = subprocess.run(
            [str(binary), "key", "check"],
            env=env,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=5 * 60,
        )
        returncode, output = proc.returncode, proc.stdout + proc.stderr
    except (OSError, subprocess.TimeoutExpired) as error:
        returncode, output = -1, f"key check did not finish: {error}"
    (scratch / "key-check.txt").write_text(output, encoding="utf-8")
    ok, reason = model_check_verdict(returncode, output)
    return {"ok": ok, "reason": reason, "returncode": returncode}


def cmd_run(args: argparse.Namespace) -> int:
    scenario = load_scenario()
    claude = shutil.which(args.claude) or args.claude
    source_binary = Path(args.anamnesis).resolve()
    if not source_binary.exists():
        raise SystemExit(f"no anamnesis binary at {source_binary}")

    run_id = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = Path(args.root) / "runs" / run_id
    run_dir.mkdir(parents=True)

    # Before the binary is copied, because a copy nothing else can see is the
    # first thing that goes wrong and the last thing that gets blamed.
    if reason := redirected(run_dir):
        print(f"  {reason}", file=sys.stderr)
        return 4
    arms = [arm for arm in ("memory", "control") if arm in args.arms.split(",")]
    only = set(args.only.split(",")) if args.only else None

    # A copy, so that an upgrade of the binary it came from during a two-hour
    # run changes nothing about this one.
    binary = run_dir / "bin" / source_binary.name
    binary.parent.mkdir()
    shutil.copy2(source_binary, binary)

    results = {
        "run": run_id,
        "scenario": scenario["name"],
        "model": args.model,
        "claude": version_of([claude, "--version"]),
        "anamnesis": version_of([str(binary), "--version"]),
        "python": sys.version.split()[0],
        "platform": f"{platform.system()} {platform.release()}",
        "arms": arms,
        "started": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "sessions": [],
        "complete": False,
    }
    results_path = run_dir / "results.json"
    write_json(results_path, results)
    print(f"run {run_id}: {', '.join(arms)} on {results['claude']}, {results['anamnesis']}")

    # First, because it is the cheapest thing that can make the whole run
    # worthless, and the isolation check below already spends an agent call.
    settings = Path(args.settings_env) if args.settings_env else live_data_dir() / "settings.env"
    if "memory" in arms and args.settings_env != "none" and not args.skip_model_check:
        if not settings.exists():
            print(f"  no settings.env at {settings}; the memory arm would run without a model", file=sys.stderr)
            return 3
        checked = model_check(binary, settings, run_dir / "model-check")
        results["model_check"] = checked
        write_json(results_path, results)
        print(f"  model: {checked['reason']}")
        if not checked["ok"]:
            print(
                "  the memory arm's model cannot be shown to work, so every page would be counted and "
                f"every probe excluded; stopping. See {run_dir / 'model-check' / 'key-check.txt'}",
                file=sys.stderr,
            )
            return 3

    if not args.skip_isolation_check:
        isolation = isolation_check(claude, args.model, run_dir / "isolation")
        results["isolation"] = isolation
        write_json(results_path, results)
        print(f"  isolation: {isolation['answer']!r}")
        if not isolation["isolated"]:
            print("  the control setup reports memory of its own; stopping", file=sys.stderr)
            return 2

    repos: dict[str, Path] = {}
    server = None
    stopped: str | None = None
    project = f"ledger-{run_id.lower()}"
    try:
        for arm in arms:
            arm_dir = run_dir / arm
            (arm_dir / "sessions").mkdir(parents=True)
            repos[arm] = arm_dir / "repo"
            prepare_repo(repos[arm], project if arm == "memory" else None)

        if "memory" in arms:
            data = run_dir / "memory" / "data"
            data.mkdir()
            if args.settings_env != "none" and settings.exists():
                shutil.copy2(settings, data / "settings.env")
                results["settings_env"] = str(settings)
                results["consolidation"] = consolidation_model(settings)
            server = Server(binary, data, args.port, run_dir / "memory" / "server.log")
            server.start()
            wired = subprocess.run(
                [str(binary), "setup", "--write", "--no-service", "--no-seed", "--port", str(args.port)],
                cwd=repos["memory"],
                env=server.env(),
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
            (run_dir / "memory" / "setup.txt").write_text(wired.stdout + wired.stderr, encoding="utf-8")
            if wired.returncode != 0 or not (repos["memory"] / ".mcp.json").exists():
                raise SystemExit(f"setup did not wire the memory arm; see {run_dir / 'memory' / 'setup.txt'}")
            status = subprocess.run(
                [str(binary), "status"],
                cwd=repos["memory"],
                env=server.env(),
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
            (run_dir / "memory" / "status.txt").write_text(status.stdout + status.stderr, encoding="utf-8")

        for session in scenario["session"]:
            if only and session["id"] not in only:
                continue
            for arm in arms:
                record = run_one(args, claude, scenario, session, arm, repos[arm], run_dir, project)
                results["sessions"].append(record)
                write_json(results_path, results)
                verdict = "pass" if record["check"]["passed"] else "FAIL"
                source = f", page {record['page']['source']}" if arm == "memory" else ""
                refused = record["agent"]["permission_denials"] or 0
                print(
                    f"  {session['id']} {arm:<7} {verdict}  turns {record['agent']['turns']}, "
                    f"${record['agent']['cost_usd'] or 0:.3f}, memory calls {record['agent']['memory_calls']}"
                    f"{f', {refused} refused' if refused else ''}{source}"
                )
                if not args.keep_going and (reason := nothing_left_to_measure(session, arm, record)):
                    stopped = reason
                    break
            if stopped:
                break
        results["complete"] = not only and not stopped
    finally:
        if server:
            server.stop()
        if stopped:
            results["stopped"] = stopped
        results["finished"] = dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds")
        write_json(results_path, results)
    print(f"results: {results_path}")
    if stopped:
        print(
            f"  stopping: {stopped}. The memory arm's model is not writing pages; see "
            f"{run_dir / 'memory' / 'server.log'} for what it answered. `--keep-going` runs anyway.",
            file=sys.stderr,
        )
        return 5
    return 0


def run_one(args, claude: str, scenario: dict, session: dict, arm: str, repo: Path, run_dir: Path, project: str) -> dict:
    data = run_dir / "memory" / "data"
    server_log = run_dir / "memory" / "server.log"
    log_from = log_size(server_log)
    mcp_config = repo / ".mcp.json" if arm == "memory" else None
    log = run_dir / arm / "sessions" / f"{session['id']}.jsonl"
    agent = run_claude(claude, repo, session["prompt"], args.model, args.max_turns, mcp_config, log)
    page = wait_for_page(data, project, agent["claude_session"], server_log, log_from) if arm == "memory" else None
    diff = commit_session(repo, session["id"])
    verdict = checks.CHECKS[session["check"]](repo)
    return {
        "session": session["id"],
        "kind": session["kind"],
        "arm": arm,
        "check_name": session["check"],
        "check": verdict.as_dict(),
        "agent": agent,
        "page": page,
        "diff": diff[-1500:],
    }


# ---------------------------------------------------------------------------
# Report


def cmd_report(args: argparse.Namespace) -> int:
    scenario = load_scenario()
    runs = []
    for path in sorted((Path(args.root) / "runs").glob("*/results.json")):
        runs.append(json.loads(path.read_text(encoding="utf-8")))
    if not runs:
        print(f"no runs under {Path(args.root) / 'runs'}")
        return 1

    lines = report_lines(scenario, runs)
    text = "\n".join(lines) + "\n"
    print(text)
    if args.markdown:
        Path(args.markdown).write_text(text, encoding="utf-8")
    return 0


def report_lines(scenario: dict, runs: list[dict]) -> list[str]:
    by = {}
    for run in runs:
        for record in run["sessions"]:
            by.setdefault((record["session"], record["arm"]), []).append((run, record))

    def valid(run: dict, record: dict) -> bool:
        """A memory-arm result counts only if every page it could have learnt
        from was written by a model and the MCP server was up."""
        if record["arm"] != "memory":
            return True
        needs = next(s for s in scenario["session"] if s["id"] == record["session"]).get("needs")
        earlier = [r for r in run["sessions"] if r["arm"] == "memory" and r["session"] < record["session"]]
        if record["agent"]["mcp_servers"].get("anamnesis") != "connected":
            return False
        if needs:
            planted = [r for r in earlier if r["session"] == needs]
            if not planted or (planted[0]["page"] or {}).get("source") != "model":
                return False
        return True

    complete = [run for run in runs if run.get("complete")]
    lines = [
        f"# Long-run memory eval: {scenario['name']}",
        "",
        f"{len(runs)} run(s), {len(complete)} complete. Agent model: "
        + ", ".join(sorted({run['model'] for run in runs}))
        + ".",
        "",
        "## Probes",
        "",
        "| Probe | Needs | Memory passed | Control passed | Memory calls (mean) |",
        "|---|---|---|---|---|",
    ]
    for session in scenario["session"]:
        if session["kind"] != "probe":
            continue
        memory = [(run, record) for run, record in by.get((session["id"], "memory"), [])]
        control = [record for _, record in by.get((session["id"], "control"), [])]
        counted = [record for run, record in memory if valid(run, record)]
        excluded = len(memory) - len(counted)
        mem_pass = sum(record["check"]["passed"] for record in counted)
        con_pass = sum(record["check"]["passed"] for record in control)
        calls = [record["agent"]["memory_calls"] or 0 for _, record in memory]
        mem_cell = f"{mem_pass}/{len(counted)}" + (f" ({excluded} excluded)" if excluded else "")
        lines.append(
            f"| {session['id']} {session['check']} | {session.get('needs', '')} | {mem_cell} | "
            f"{con_pass}/{len(control)} | {mean(calls)} |"
        )

    lines += [
        "",
        "## Effort per session",
        "",
        "| Session | Kind | Arm | Turns (mean) | Cost USD (mean) | Refused (mean) | Task passed |",
        "|---|---|---|---|---|---|---|",
    ]
    for session in scenario["session"]:
        for arm in ("memory", "control"):
            records = [record for _, record in by.get((session["id"], arm), [])]
            if not records:
                continue
            lines.append(
                f"| {session['id']} | {session['kind']} | {arm} | "
                f"{mean([r['agent']['turns'] or 0 for r in records])} | "
                f"{mean([r['agent']['cost_usd'] or 0 for r in records], 4)} | "
                f"{mean([r['agent']['permission_denials'] or 0 for r in records])} | "
                f"{sum(r['check']['passed'] for r in records)}/{len(records)} |"
            )

    pages = Counter((record["page"] or {}).get("source", "none") for run in runs for record in run["sessions"] if record["arm"] == "memory")
    lines += [
        "",
        "## Validity",
        "",
        f"Memory-arm pages: {dict(pages)}. A probe whose planting session's page was counted, "
        "or that ran without the MCP server connected, is excluded from the memory column above.",
    ]
    wrote_with = {run["consolidation"] for run in runs if run.get("consolidation")}
    if len(wrote_with) > 1:
        lines.append(
            f"**The memory arm did not write its pages with one model: {sorted(wrote_with)}.** "
            "Those runs are two experiments and the columns above pool them."
        )
    # Counted from `permission_denials`, which every run has, and broken down
    # only for the runs that recorded which tool was refused.
    refused = sum((record["agent"]["permission_denials"] or 0) for run in runs for record in run["sessions"])
    by_tool = Counter()
    for run in runs:
        for record in run["sessions"]:
            by_tool.update(record["agent"].get("denials_by_tool") or {})
    if refused:
        breakdown = f" ({dict(by_tool.most_common())})" if by_tool else ""
        lines.append(
            f"Refused tool calls: {refused}{breakdown}. A refusal costs the session a turn and does "
            "not fall equally on the two arms, so a probe that failed after several of them says "
            "more about this harness than about memory."
        )
    for run in runs:
        isolation = run.get("isolation", {}).get("answer", "not checked")
        stopped = f", stopped: {run['stopped']}" if run.get("stopped") else ""
        wrote_with = f", pages by {run['consolidation']}" if run.get("consolidation") else ""
        lines.append(
            f"- {run['run']}: {'complete' if run.get('complete') else 'incomplete'}, "
            f"isolation {isolation!r}, {run['anamnesis']}{wrote_with}{stopped}"
        )
    return lines


def mean(values: list, places: int = 1):
    return round(sum(values) / len(values), places) if values else "-"


# ---------------------------------------------------------------------------


def cmd_selftest(_: argparse.Namespace) -> int:
    scenario = load_scenario()
    print(f"scenario {scenario['name']}: {len(scenario['session'])} sessions, every check known")
    sample = "\n".join(
        [
            json.dumps({"type": "system", "subtype": "init", "session_id": "abc", "mcp_servers": [{"name": "anamnesis", "status": "connected"}]}),
            json.dumps({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "mcp__anamnesis__memory_query"}, {"type": "tool_use", "name": "Read"}]}}),
            json.dumps({"type": "user", "message": {"content": [{"type": "tool_result", "is_error": True}]}}),
            json.dumps(
                {
                    "type": "result",
                    "num_turns": 3,
                    "total_cost_usd": 0.01,
                    "result": "done",
                    "usage": {"input_tokens": 5, "cache_read_input_tokens": 10},
                    "permission_denials": [
                        {"tool_name": "Bash", "tool_input": {"command": "rm x.py"}},
                        {"tool_name": "Bash", "tool_input": {"command": "pip install pytest"}},
                        {"tool_name": "PowerShell", "tool_input": {"command": "Get-ChildItem"}},
                    ],
                }
            ),
        ]
    )
    summary = summarize_stream(sample)
    expected = {
        "memory_calls": 1,
        "tool_errors": 1,
        "turns": 3,
        "input_tokens": 15,
        "mcp_servers": {"anamnesis": "connected"},
        "permission_denials": 3,
        "denials_by_tool": {"Bash": 2, "PowerShell": 1},
    }
    wrong = {key: summary[key] for key, value in expected.items() if summary[key] != value}
    if wrong:
        print(f"FAIL summarize_stream: {wrong}")
        return 1
    print("ok   summarize_stream reads turns, tokens, tool errors, memory calls and what was refused")

    # A session that is given a shell it may not use spends turns finding that
    # out, so the tool is taken away rather than refused. Both arms are
    # started with the same flags, and this is the one that says so.
    args = claude_args("claude", DEFAULT_MODEL, MAX_TURNS, None)
    if "--disallowedTools" not in args or "PowerShell" not in args[args.index("--disallowedTools") + 1]:
        print("FAIL claude_args: the session is still given a PowerShell tool it may not use")
        return 1
    if any("PowerShell" in rule for rule in ALLOWED_TOOLS):
        print("FAIL ALLOWED_TOOLS: a PowerShell rule allows every PowerShell command, not the one it names")
        return 1
    print("ok   claude_args takes the PowerShell tool away instead of refusing it")

    plant = {"id": "S01", "kind": "plant", "plants": "S12"}
    probe = {"id": "S12", "kind": "probe"}
    stops = [
        (plant, "memory", "model", False),
        (plant, "memory", "counted", True),
        (plant, "memory", "none", True),
        # A probe's own page is not what a later probe reads, and the control
        # arm has no pages at all; neither ends a run.
        (probe, "memory", "counted", False),
        (plant, "control", None, False),
    ]
    for session, arm, source, expected in stops:
        record = {"page": {"source": source} if source else None}
        got = nothing_left_to_measure(session, arm, record) is not None
        if got != expected:
            print(f"FAIL nothing_left_to_measure({session['id']}, {arm}, {source}): {got}, expected {expected}")
            return 1
    print("ok   a run stops when a planting session's page was written by counting")

    # What `key check` printed on this machine on 2026-09-15, and the two
    # other things it can come back as.
    refused = (
        "🔑 Checking the model key\n\n"
        "  model    gemini-3.5-flash (https://generativelanguage.googleapis.com/v1beta/openai)\n"
        "  key      ANAMNESIS_LLM_API_KEY, from the credential store\n"
        "  ❌ the key was refused (400): Please pass a valid API key\n\n"
        "Error: 1 of 1 model(s) could not be shown to work\n"
    )
    cases = [
        ((0, "  ✅ answered, as gemini-3.5-flash: the key works\n"), (True, "every model answered")),
        ((1, refused), (False, "the key was refused (400): Please pass a valid API key")),
        (
            (1, "  ⚠️  the service did not answer (503): overloaded — this says nothing about the key; try again\n"),
            (False, "the service did not answer (503): overloaded — this says nothing about the key; try again"),
        ),
        (
            (2, "error: unrecognized subcommand 'check'\n\nUsage: anamnesis key <COMMAND>\n"),
            (False, "this anamnesis has no `key check`; use a newer build or pass --skip-model-check"),
        ),
    ]
    for (returncode, output), expected in cases:
        got = model_check_verdict(returncode, output)
        if got != expected:
            print(f"FAIL model_check_verdict({returncode}): {got!r}, expected {expected!r}")
            return 1
    print("ok   model_check_verdict stops a run on a refused key and on a binary without key check")

    here = Path("C:/Users/x/AppData/Local/anamnesis-longrun/runs/r/.probe")
    elsewhere = Path(
        "C:/Users/x/AppData/Local/Packages/PythonSoftwareFoundation.Python.3.11_qbz5n2kfra8p0"
        "/LocalCache/Local/anamnesis-longrun/runs/r/.probe"
    )
    if redirection_reason(here, here) is not None:
        print("FAIL redirection_reason: a path that is where it says it is was called redirected")
        return 1
    # Only where the filesystem says so: `os.path.normcase` folds case on
    # Windows and is the identity on POSIX, where two spellings really are two
    # paths. CI runs this selftest on ubuntu.
    if os.name == "nt" and redirection_reason(here, Path(str(here).upper())) is not None:
        print("FAIL redirection_reason: Windows case difference read as redirection")
        return 1
    reason = redirection_reason(here, elsewhere)
    if reason is None or "LocalCache" not in reason:
        print(f"FAIL redirection_reason: a redirected path was not reported: {reason!r}")
        return 1
    print("ok   redirection_reason catches a Store Python writing the run somewhere else")
    return checks.selftest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)

    selftest = commands.add_parser("selftest", help="check the checks against right and wrong answers")
    selftest.set_defaults(func=cmd_selftest)

    run = commands.add_parser("run", help="one repeat of the scenario, both arms")
    run.add_argument("--anamnesis", required=True, help="the anamnesis binary the memory arm runs")
    run.add_argument("--claude", default="claude")
    run.add_argument("--model", default=DEFAULT_MODEL)
    run.add_argument("--root", default=str(default_root()))
    run.add_argument("--port", type=int, default=DEFAULT_PORT)
    run.add_argument("--max-turns", type=int, default=MAX_TURNS)
    run.add_argument("--arms", default="memory,control")
    run.add_argument("--only", help="comma-separated session ids, for trying the harness out")
    run.add_argument(
        "--settings-env",
        help="settings.env for the memory arm's server; this machine's by default, 'none' for no model",
    )
    run.add_argument("--skip-isolation-check", action="store_true")
    run.add_argument(
        "--keep-going",
        action="store_true",
        help="run every session even after a planting session's page was written by counting",
    )
    run.add_argument(
        "--skip-model-check",
        action="store_true",
        help="start without asking the memory arm's model first (`anamnesis key check`)",
    )
    run.set_defaults(func=cmd_run)

    report = commands.add_parser("report", help="every run so far")
    report.add_argument("--root", default=str(default_root()))
    report.add_argument("--markdown", help="also write the report here")
    report.set_defaults(func=cmd_report)

    args = parser.parse_args()
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
