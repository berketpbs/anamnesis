"""Plan isolated client invocations; execute one explicitly released trial."""
import argparse
import datetime as dt
import hashlib
import json
import os
import queue
import shutil
import signal
import subprocess
import threading
import time
from pathlib import Path

from comparison import load, write_json

ACTIONS = {"command_execution", "file_change", "mcp_tool_call", "web_search"}


def client_plan(root: Path, trial_id: str, executable: str, *, anamnesis=None):
    manifest = load(root)
    trial = next(t for t in manifest["trials"] if t["id"] == trial_id)
    repo, profile = (root / trial["repo"]).resolve(), (root / trial["profile"]).resolve()
    if not repo.is_relative_to(root.resolve()) or not profile.is_relative_to(root.resolve()):
        raise ValueError("client workspace/profile escapes the run")
    if trial["method"] != "history" and not (repo / "durum.md").is_file():
        raise ValueError("brief and Anamnesis arms require their prepared durum.md")
    if trial["method"] == "anamnesis" and anamnesis is None:
        raise ValueError("Anamnesis arm needs an explicit binary and independently prepared store/hooks")
    if trial["method"] != "anamnesis" and anamnesis is not None:
        raise ValueError("control arms cannot receive an Anamnesis MCP server")
    # Preserve tool/runtime discovery, excluding all host model/provider/plugin
    # variables. Profile paths apply to the client and its children only.
    allowed = {"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "COMSPEC", "TEMP", "TMP",
               "LOCALAPPDATA", "APPDATA", "LANG", "LC_ALL"}
    env = {k: v for k, v in os.environ.items() if k.upper() in allowed}
    env.update(CODEX_HOME=str(profile / "codex"), CLAUDE_CONFIG_DIR=str(profile / "claude"),
               HOME=str(profile), USERPROFILE=str(profile), CLAUDE_CODE_DISABLE_AUTO_MEMORY="1",
               ANAMNESIS_DATA_DIR=str(profile / "anamnesis-data"), ANAMNESIS_LLM_PROVIDER="none",
               ANAMNESIS_EMBED_ENABLED="0", ANAMNESIS_KEY_SERVICE="anamnesis-continuity-isolated")
    mcp = {}
    if anamnesis is not None:
        mcp = {"anamnesis": dict(command=str(Path(anamnesis).resolve()),
            args=["--data-dir", str(profile / "anamnesis-data"), "mcp", "--repo", str(repo)],
            env=dict(ANAMNESIS_LLM_PROVIDER="none", ANAMNESIS_EMBED_ENABLED="0",
                     ANAMNESIS_KEY_SERVICE="anamnesis-continuity-isolated"))}
    if trial["reader"] == "codex":
        profile_hooks = profile / "codex/hooks.json"
        if trial["method"] != "anamnesis" and profile_hooks.exists():
            raise ValueError("a control profile may not contain Anamnesis or other lifecycle hooks")
        argv = [executable, "--no-daemon", "exec", "--json", "--skip-git-repo-check",
                "--ignore-user-config", "--ignore-rules", "-m", trial["model"],
                "-s", "workspace-write", "-C", str(repo)]
        if trial["method"] == "anamnesis":
            if not profile_hooks.is_file():
                raise ValueError("Codex Anamnesis arm requires prepared hooks in its isolated CODEX_HOME")
            # Ignoring this owned layer also skips its lifecycle hooks. Host
            # configuration remains excluded by the separate CODEX_HOME.
            argv.remove("--ignore-user-config")
        overrides = ["features.memories=false", "features.memory_tool=false", "agents.enabled=false",
                     'approval_policy="never"', "features.hooks=true"]
        if os.name == "nt":
            overrides.append('windows.sandbox="unelevated"')
        for name, server in mcp.items():
            overrides += [f"mcp_servers.{name}.command={json.dumps(server['command'])}",
                          f"mcp_servers.{name}.args={json.dumps(server['args'])}",
                          f"mcp_servers.{name}.required=true"]
            overrides += [f"mcp_servers.{name}.env.{key}={json.dumps(value)}"
                          for key, value in server["env"].items()]
        for option in overrides:
            argv += ["-c", option]
        argv += ["-"]
    else:
        argv = [executable, "-p", "--model", trial["model"], "--output-format", "stream-json",
                "--verbose", "--restricted", "--setting-sources", "project,local", "--strict-mcp-config",
                "--mcp-config", json.dumps(dict(mcpServers=mcp)), "--disable-slash-commands",
                "--tools", "Read,Edit,Write,Glob,Grep,Bash", "--permission-mode", "acceptEdits",
                "--allowedTools", "Read,Edit,Write,Glob,Grep,Bash,mcp__anamnesis__*",
                "--disallowedTools", "Agent,Task,WebSearch,WebFetch,PowerShell"]
        if trial["method"] == "anamnesis":
            settings = repo / ".claude/settings.local.json"
            if not settings.is_file():
                raise ValueError("Claude Anamnesis arm requires explicitly prepared local hook settings")
            argv += ["--settings", str(settings)]
    prompt = trial["prompt"] + "\nResearch archive/ and local-memory/ for the task's prior context."
    if trial["method"] != "history":
        prompt += "\nA prepared continuity brief is available at durum.md."
    return dict(trial=trial, argv=argv, env=env, cwd=repo, prompt=prompt)


class StreamMeasurements:
    def __init__(self, reader):
        self.reader = reader
        self.actions = set()
        self.tokens = None
        self.cost = None
        self.complete = False
        self.failed = False
        self.reported_model = None

    def consume(self, line):
        try:
            event = json.loads(line)
        except (ValueError, TypeError):
            return
        kind = event.get("type")
        if self.reader == "codex":
            item = event.get("item", {})
            if kind in ("item.started", "item.completed") and item.get("type") in ACTIONS:
                self.actions.add(item.get("id") or hashlib.sha256(json.dumps(item, sort_keys=True).encode()).hexdigest())
            if kind == "turn.completed":
                usage = event.get("usage", {})
                if "input_tokens" in usage and "output_tokens" in usage:
                    self.tokens = (self.tokens or 0) + usage["input_tokens"] + usage["output_tokens"]
                self.complete, self.failed = True, False
            elif kind == "turn.failed":
                self.failed = True
        else:
            if kind == "system" and event.get("subtype") == "init":
                self.reported_model = event.get("model")
            if kind == "assistant":
                for block in event.get("message", {}).get("content", []) or []:
                    if block.get("type") == "tool_use":
                        self.actions.add(block["id"])
            if kind == "result":
                self.complete = event.get("subtype") == "success" and not event.get("is_error", False)
                self.failed = not self.complete
                usage = event.get("usage", {})
                if "input_tokens" in usage and "output_tokens" in usage:
                    self.tokens = sum(usage.get(key, 0) for key in (
                        "input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"))
                self.cost = event.get("total_cost_usd")


def stop_owned_process(process):
    if process.poll() is not None:
        return
    if os.name == "nt":
        subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                       creationflags=subprocess.CREATE_NO_WINDOW, check=False)
    else:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    process.wait(timeout=10)


def execute(plan, *, claude_ready=False, readiness=None, now=None):
    trial = plan["trial"]
    if trial["reader"] == "claude":
        current = now or dt.datetime.now(dt.timezone.utc)
        if not claude_ready or current < dt.datetime(2026, 10, 3, 16, tzinfo=dt.timezone.utc):
            raise ValueError("Claude runs remain paused until 19:00 Istanbul and verified quota availability")
    if not readiness or readiness.get("reader") != trial["reader"] or readiness.get("model") != trial["model"]:
        raise ValueError("verified client readiness for the frozen reader/model is required")
    for field in ("access_verified", "profile_isolation_verified", "permissions_verified", "treatment_verified"):
        if readiness.get(field) is not True:
            raise ValueError(f"client readiness is missing {field}")
    # This attestation is an external acceptance record, not generated proof.
    if not readiness.get("evidence"):
        raise ValueError("client readiness must cite fresh-process verification evidence")
    trial_dir = plan["cwd"].parent
    dispatch = trial_dir / "dispatch.json"
    write_json(dispatch, dict(reader=trial["reader"], model=trial["model"], argv=plan["argv"],
                             readiness=readiness, prompt_sha256=hashlib.sha256(plan["prompt"].encode()).hexdigest()))
    events = queue.Queue()
    measured = StreamMeasurements(trial["reader"])
    kwargs = dict(cwd=plan["cwd"], env=plan["env"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                  text=True, encoding="utf-8", errors="replace", bufsize=1)
    kwargs.update(dict(creationflags=subprocess.CREATE_NO_WINDOW) if os.name == "nt" else dict(start_new_session=True))
    started = time.monotonic()
    status = "error"
    with (trial_dir / "client.stderr.log").open("x", encoding="utf-8") as stderr, \
            (trial_dir / "client.events.jsonl").open("x", encoding="utf-8") as transcript:
        process = subprocess.Popen(plan["argv"], stderr=stderr, **kwargs)
        def read_events():
            for line in process.stdout:
                events.put(line)
            events.put(None)
        reader = threading.Thread(target=read_events, daemon=True)
        reader.start()
        try:
            process.stdin.write(plan["prompt"])
            process.stdin.close()
            while True:
                if time.monotonic() - started >= trial["budget"]["seconds"]:
                    status = "budget"
                    break
                try:
                    line = events.get(timeout=.1)
                except queue.Empty:
                    continue
                if line is None:
                    process.wait(timeout=10)
                    status = "complete" if process.returncode == 0 and measured.complete and not measured.failed else "error"
                    break
                transcript.write(line)
                transcript.flush()
                measured.consume(line)
                if len(measured.actions) >= trial["budget"]["actions"]:
                    status = "budget"
                    break
        finally:
            stop_owned_process(process)
            reader.join(timeout=5)
            process.stdout.close()
            if not process.stdin.closed:
                process.stdin.close()
            while not events.empty():
                line = events.get_nowait()
                if line is not None:
                    transcript.write(line)
                    measured.consume(line)
    # Do not claim capture/summary/delivery/source-open from tool names alone.
    result = dict(reader=trial["reader"], model=trial["model"], status=status,
        elapsed_seconds=time.monotonic() - started, tool_calls=len(measured.actions), tokens=measured.tokens,
        agent_cost_usd=measured.cost, captured=None, kept=None, shown=None, source_opened=None,
        reported_model=measured.reported_model, preparation_seconds=None, preparation_tokens=None,
        user_reminders=None, evidence_status="supply measured preparation/reminders and stage traces before record")
    if measured.reported_model is not None and measured.reported_model != trial["model"]:
        result["status"] = "error"
        result["evidence_status"] = "reported model differs from frozen reader; no fallback result accepted"
    write_json(trial_dir / "client.measurements.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--trial", required=True)
    parser.add_argument("--client", required=True, help="installed executable path or command name")
    parser.add_argument("--anamnesis", type=Path)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--claude-ready", action="store_true")
    parser.add_argument("--readiness", type=Path)
    args = parser.parse_args()
    executable = shutil.which(args.client)
    if not executable:
        raise SystemExit("client executable is unavailable")
    plan = client_plan(args.root, args.trial, executable, anamnesis=args.anamnesis)
    if args.execute:
        ready = json.loads(args.readiness.read_text(encoding="utf-8")) if args.readiness else None
        print(json.dumps(execute(plan, claude_ready=args.claude_ready, readiness=ready)))
    else:
        print(json.dumps(dict(argv=plan["argv"], cwd=str(plan["cwd"]), prompt=plan["prompt"],
                              profile=str(args.root / plan["trial"]["profile"]), executed=False), indent=2))


if __name__ == "__main__":
    main()
