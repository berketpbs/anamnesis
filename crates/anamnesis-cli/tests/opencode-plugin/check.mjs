// The OpenCode plugin, run the way OpenCode runs it, against a real server.
//
// `anamnesis install-hooks --agent opencode` writes a JavaScript file for Bun,
// and until this check nothing had ever loaded it: the Rust tests assert on the
// text of the template, which says nothing about whether the file parses, what
// its handlers send, or whether the hook command and the server accept it.
//
// Usage: bun check.mjs <plugin.js> <project directory> <anamnesis binary> <data dir>
//
// The plugin is the file `install-hooks` wrote, pointed at a server the caller
// started. Two OpenCode runs are played through it: the first does some work
// and ends, the second starts and must be handed what the first left. What the
// server recorded is then read back with the binary's own `sessions` command.

const [pluginPath, project, binary, dataDir] = process.argv.slice(2);
if (!pluginPath || !project || !binary || !dataDir) {
  console.error("usage: bun check.mjs <plugin.js> <project> <anamnesis> <data dir>");
  process.exit(2);
}

let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures += 1;
};

const { Anamnesis } = await import(pluginPath);
check(typeof Anamnesis === "function", "the plugin exports its entry point");

// The first run: a prompt, a tool call, a compaction, and the end.
const first = await Anamnesis({ directory: project, worktree: project });
for (const hook of [
  "chat.message",
  "tool.execute.after",
  "experimental.session.compacting",
  "experimental.chat.system.transform",
  "dispose",
]) {
  check(typeof first[hook] === "function", `the plugin answers ${hook}`);
}

await first["chat.message"](
  { sessionID: "opencode-session-one" },
  { parts: [{ type: "text", text: "Make the parser accept a trailing comma." }] },
);
await first["tool.execute.after"](
  { sessionID: "opencode-session-one", tool: "bash", args: { command: "cargo test -p parser" } },
  { title: "cargo test -p parser", output: "test result: ok. 12 passed; 0 failed" },
);
await first["experimental.session.compacting"]({ sessionID: "opencode-session-one" });
await first.dispose();

// The second run: the first thing that reaches the model is the note.
const second = await Anamnesis({ directory: project, worktree: project });
const prompt = { system: [] };
await second["experimental.chat.system.transform"]({ sessionID: "opencode-session-two" }, prompt);
check(prompt.system.length === 1, "a new session is handed one note");
check(
  (prompt.system[0] ?? "").startsWith("# Memory from earlier sessions (anamnesis)"),
  "the note is labelled as memory, not as an instruction",
);

const again = { system: [] };
await second["experimental.chat.system.transform"]({ sessionID: "opencode-session-two" }, again);
check(again.system.length === 0, "the note is handed once per session");
await second.dispose();

// What the server made of it, in the binary's own words.
const listed = Bun.spawnSync([binary, "--data-dir", dataDir, "sessions"], { cwd: project });
const sessions = new TextDecoder().decode(listed.stdout);
console.log(sessions.trim());
const rows = sessions.trim().split("\n").filter((line) => line.includes("opencode"));
check(rows.length === 2, "the server recorded both runs as opencode sessions");
const closed = rows.filter((line) => /\bclosed\b/.test(line));
check(closed.length === 2, "and both were closed by the plugin's dispose");
const observed = rows.map((line) => Number((line.match(/(\d+) obs/) ?? [])[1] ?? 0));
check(Math.max(...observed) >= 4, "the working run kept its prompt, tool call, compaction and end");

if (failures > 0) {
  console.error(`${failures} check(s) failed`);
  process.exit(1);
}
console.log("the plugin works end to end");
