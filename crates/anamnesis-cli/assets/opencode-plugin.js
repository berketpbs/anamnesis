// anamnesis — long-term memory for AI coding agents.
//
// Written by `anamnesis install-hooks --agent opencode`. Running that command
// again rewrites this file, so edit it only if you are prepared to lose the
// edit; everything configurable is either baked in below or read from the
// environment at run time.
//
// OpenCode extends through a plugin API rather than through command hooks, so
// this file does what a hook command does elsewhere: it forwards the five
// lifecycle moments to the memory server, and it hands a starting session what
// the last one left.
//
// Two of the hooks used here are marked experimental by OpenCode
// (`experimental.session.compacting` and `experimental.chat.system.transform`).
// If a future version renames them, capture keeps working — what stops is the
// pre-compact event and the handoff arriving on its own. Everything the
// session did is still recorded.

const BINARY = {{BINARY}};
const SERVER = {{SERVER}};

// How much of a tool's output travels with the event. The server keeps 16 KB
// of any body and cuts the rest, so sending more only costs the pipe; sending
// some is what lets a summary say whether the command worked.
const TOOL_OUTPUT_LIMIT = 4096;

// The handoff is fetched while somebody is waiting for their first reply, so
// it gets the same budget the hook command uses: enough for a local server,
// short enough that a dead one is not felt.
const HANDOFF_TIMEOUT_MS = 2000;

export const Anamnesis = async ({ directory, worktree }) => {
  const cwd = worktree ?? directory ?? process.cwd();

  // Sessions this plugin has already opened, and already handed a note to.
  // OpenCode has no "session started" hook a plugin reliably receives, so the
  // start is synthesised from the first event that carries a session id — and
  // a set is what keeps that from being sent again on the second.
  const started = new Set();
  const handed = new Set();
  let last = null;

  // Everything here is best-effort. A memory system that can break somebody's
  // editing session is worse than one that occasionally misses an event, which
  // is why the hook command exits zero on every failure and why nothing below
  // is allowed to throw.
  const send = async (payload) => {
    try {
      const child = Bun.spawn(
        [BINARY, "hook", "--agent", "opencode", "--server", SERVER],
        { stdin: "pipe", stdout: "ignore", stderr: "ignore" },
      );
      child.stdin.write(JSON.stringify(payload));
      child.stdin.end();
      await child.exited;
    } catch {
      // The binary is missing or unrunnable. `anamnesis status` says so; there
      // is nothing useful to do from inside somebody's editor.
    }
  };

  const open = async (sessionID) => {
    if (!sessionID) return;
    last = sessionID;
    if (started.has(sessionID)) return;
    started.add(sessionID);
    await send({
      session_id: sessionID,
      hook_event_name: "SessionStart",
      cwd,
      source: "startup",
    });
  };

  const claim = async (sessionID) => {
    try {
      const url =
        `${SERVER}/handoff?agent=opencode` +
        `&session_id=${encodeURIComponent(sessionID)}` +
        `&cwd=${encodeURIComponent(cwd)}`;
      const headers = {};
      const token = process.env.ANAMNESIS_TOKEN;
      if (token) headers.authorization = `Bearer ${token}`;
      const response = await fetch(url, {
        headers,
        signal: AbortSignal.timeout(HANDOFF_TIMEOUT_MS),
      });
      if (!response.ok) return null;
      const text = (await response.text()).trim();
      return text.length > 0 ? text : null;
    } catch {
      return null;
    }
  };

  return {
    // What the person asked for. The text lives in the message's parts, so it
    // is joined back into one prompt rather than sent as a structure nothing
    // downstream reads.
    "chat.message": async (input, output) => {
      await open(input.sessionID);
      const prompt = (output.parts ?? [])
        .filter((part) => part.type === "text" && typeof part.text === "string")
        .map((part) => part.text)
        .join("\n");
      await send({
        session_id: input.sessionID,
        hook_event_name: "UserPromptSubmit",
        cwd,
        prompt,
      });
    },

    // What the agent did about it. `after` rather than `before`: the outcome is
    // the half a summary can use, and a tool call that never ran is not a thing
    // that happened.
    "tool.execute.after": async (input, output) => {
      await open(input.sessionID);
      await send({
        session_id: input.sessionID,
        hook_event_name: "PostToolUse",
        cwd,
        tool_name: input.tool,
        tool_input: input.args ?? {},
        tool_response: {
          title: output.title,
          output: String(output.output ?? "").slice(0, TOOL_OUTPUT_LIMIT),
        },
      });
    },

    // The session admitting it no longer fits in its own context, which is
    // exactly the moment a durable memory is worth having.
    "experimental.session.compacting": async (input) => {
      await open(input.sessionID);
      await send({
        session_id: input.sessionID,
        hook_event_name: "PreCompact",
        cwd,
        trigger: "compacting",
      });
    },

    // Delivery. Elsewhere the handoff reaches the model because the harness
    // injects a hook's stdout; OpenCode has no such channel, so the note is
    // pushed into the system prompt instead — once per session, because
    // claiming one consumes it.
    "experimental.chat.system.transform": async (input, output) => {
      const sessionID = input.sessionID;
      if (!sessionID || handed.has(sessionID)) return;
      handed.add(sessionID);
      await open(sessionID);
      const handoff = await claim(sessionID);
      if (handoff) {
        // Labelled, and labelled as anamnesis: an unmarked block of text in
        // the system prompt reads as an instruction from whoever set the
        // machine up, and this is a note from a previous session.
        output.system.push(
          `# Memory from earlier sessions (anamnesis)\n\n${handoff}`,
        );
      }
    },

    // The closest thing OpenCode has to a session ending. A session that is
    // never closed this way is summarised by the server's reaper instead, so
    // the cost of missing it is a page written late rather than not at all.
    dispose: async () => {
      if (!last) return;
      await send({
        session_id: last,
        hook_event_name: "SessionEnd",
        cwd,
        reason: "opencode exited",
      });
    },
  };
};
