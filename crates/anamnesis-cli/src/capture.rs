//! Delivering one lifecycle event, and keeping the ones that did not go.
//!
//! This is the half of anamnesis that runs inside somebody else's editing
//! session. Everything here is shaped by two facts: it runs before every tool
//! call an agent makes, and it must never be the reason a session feels
//! broken. That is why the budgets are a quarter of a second to connect and
//! one to answer, why nothing here ever exits non-zero, and why an event that
//! cannot be delivered is written to a queue instead of being dropped —
//! capture has been lost twice in this repository, and both times the only
//! evidence was that nothing had been recorded.
//!
//! The queue itself is [`crate::spool`]. What lives here is the decision of
//! what to do about a failure: which ones are worth waiting out, which ones
//! are the server's answer about this payload, and what a person is told
//! where they will actually read it.

use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;

use crate::spool;

/// Forward one hook event, and deliver the handoff when a session starts.
///
/// Never fails loudly. Hooks run inside someone's editing session, so a server
/// that is not running should cost them nothing more than a line on stderr.
/// Ask the server what it would record, and let it record nothing.
///
/// This exists because the obvious way to check that capture is working is to
/// fire a hook by hand, and the cost of that is permanent: the event is real,
/// so the session is real, and it is then counted, listed, and eventually
/// summarised into a page like anybody's afternoon. Four of the first ten
/// sessions recorded for this project were diagnostics.
///
/// Three differences from [`cmd_hook`], all of them because a person is
/// waiting on this rather than an editor:
///
/// * **It exits non-zero when memory would not record.** The hook's promise
///   to always exit 0 protects an editing session from a memory system that
///   is having a bad day; nothing here is inside anybody's editor, and a
///   diagnostic that cannot fail is not a diagnostic.
/// * **It never queues.** The queue exists so a real event survives the
///   server being down. Filling it with probes would mean the next healthy
///   hook delivers events that never happened, which is the pollution this
///   command exists to stop.
/// * **It never asks for the handoff.** A handoff is single-use. Claiming one
///   to prove memory works would consume the note the next session was owed —
///   a diagnostic that has already cost this project one.
pub fn cmd_probe(agent: &str, server: &str, token: Option<&str>) -> anyhow::Result<()> {
    let payload = probe_payload(agent)?;

    println!("🔎 Probing memory at {server}");
    println!();

    // Looser than the hook's budgets, deliberately: those are tight because a
    // hook runs before every tool call and a stall costs somebody's
    // afternoon. This runs once, with a person reading the output, where
    // calling a slow server dead is the more expensive mistake.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(5))
        .build()?;

    let mut post = client
        .post(format!("{server}/hook"))
        .query(&[("agent", agent), ("probe", "1")])
        .header("content-type", "application/json")
        .body(payload);
    if let Some(token) = token {
        post = post.bearer_auth(token);
    }

    let response = match post.send() {
        Ok(response) => response,
        Err(error) => {
            println!("  Server:     unreachable — {error}");
            println!();
            anyhow::bail!("memory is not recording: nothing is listening at {server}");
        }
    };

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().unwrap_or_default();
        println!("  Server:     reachable, and refused the event ({status})");
        println!("              {}", detail.trim());
        println!();
        anyhow::bail!("memory is not recording: the server refused a probe");
    }

    // A server older than `--probe` does not know the parameter, ignores it,
    // and records the event — answering `accepted` where a probe report
    // belongs. Read as a parse error that would be the least useful sentence
    // available, so it is named for what it is: the one case where running
    // this command leaves something behind.
    let body = response.text()?;
    let report: anamnesis_web::ProbeReport = serde_json::from_str(&body).map_err(|_| {
        anyhow::anyhow!(
            "the server answered {body:?} instead of a probe report, which means it is              older than --probe: it ignored the parameter and recorded the event. Point              the server at a current binary, then remove what this left behind with              `anamnesis forget-session`."
        )
    })?;
    println!("  Server:     reachable");
    println!("  Scope:      {}/{}", report.workspace, report.project);
    println!(
        "  Session:    {} ({})",
        &report.session[..report.session.len().min(8)],
        if report.session_known {
            "already recorded"
        } else {
            "new"
        }
    );
    println!("  Event:      {} (read as {})", report.event, report.agent);
    println!(
        "  Redacted:   {}",
        if report.redactions.is_empty() {
            "nothing".to_owned()
        } else {
            report.redactions.join(", ")
        }
    );
    println!(
        "  Handoff:    {}",
        if report.handoff_waiting {
            "one waiting — peeked, not claimed"
        } else {
            "none waiting"
        }
    );
    println!(
        "  Summaries:  {}",
        match report.consolidation {
            anamnesis_web::Consolidation::Model => "written by a model",
            anamnesis_web::Consolidation::Counted => "counted — no model configured",
        }
    );
    println!();

    match &report.excluded {
        None => {
            println!("  This event would be recorded. Nothing was.");
            Ok(())
        }
        Some(path) => {
            println!("  This event would be DROPPED: {path}");
            println!("  [capture] ignore_paths in the marker file excludes it.");
            anyhow::bail!("memory would not record this event")
        }
    }
}

/// The payload a probe sends: whatever was piped in, or one made up here.
///
/// Reading stdin only when something is piped is what keeps this usable by
/// hand — a probe that blocked on an empty terminal would be indistinguishable
/// from a server that never answers, which is the exact confusion it is meant
/// to resolve.
fn probe_payload(agent: &str) -> anyhow::Result<String> {
    use std::io::IsTerminal;

    if !std::io::stdin().is_terminal() {
        let mut piped = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut piped)?;
        let piped = piped.trim_start_matches('\u{feff}').trim().to_owned();
        if !piped.is_empty() {
            return Ok(piped);
        }
    }

    let cwd = std::env::current_dir()?;
    // Named rather than random: a probe should be recognisable as one in a
    // log, and deriving the same session identifier every time means repeated
    // probes describe one hypothetical session rather than a new one each.
    let payload = serde_json::json!({
        "hook_event_name": event_name_for(agent),
        "session_id": "anamnesis-probe",
        "cwd": cwd.to_string_lossy(),
        "prompt": "anamnesis probe: what would you do with this",
    });
    Ok(payload.to_string())
}

/// What one harness calls the event a probe imitates.
///
/// An ordinary prompt, because it is the event a session produces most and
/// the one whose path has the most to go wrong on it. Harnesses that spell it
/// differently get their own spelling: anamnesis classifies the payload
/// itself, and a name it does not recognise would be read as a notification
/// and probe a path nobody uses.
fn event_name_for(agent: &str) -> &'static str {
    match agent {
        "codex" => "user_prompt",
        "gemini-cli" => "UserPrompt",
        _ => "UserPromptSubmit",
    }
}

pub fn cmd_hook(agent: &str, server: &str, token: Option<&str>, data_dir: Option<PathBuf>) {
    let mut payload = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut payload).is_err() {
        eprintln!("anamnesis: could not read hook payload");
        return;
    }

    // Windows shells prepend a UTF-8 byte order mark when piping text into a
    // native process, and a BOM is not valid JSON. Stripping it here means the
    // hook works the same whether it was invoked from PowerShell, cmd, or a
    // POSIX shell.
    let payload = payload.trim_start_matches('\u{feff}').trim().to_owned();
    if payload.is_empty() {
        eprintln!("anamnesis: hook payload was empty");
        return;
    }

    let event = serde_json::from_str::<serde_json::Value>(&payload)
        .ok()
        .and_then(|value| {
            value
                .get("hook_event_name")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        });

    let starting = is_starting(event.as_deref(), agent);

    // This delivery's name, minted before the first attempt and reused by
    // every later one. The server records an event once under a name it has
    // seen, so a hook that gave up after a second on a server that was in fact
    // recording can offer the same event again without the session gaining a
    // prompt it never had.
    let delivery = anamnesis_core::ids::ObservationId::new().to_string();

    // Where an event waits when it cannot be delivered. Resolving the data
    // directory is path arithmetic — it opens no database and creates nothing
    // — which is what makes it affordable on a path that runs before every
    // tool call. A directory that will not resolve is not worth failing the
    // hook over: the server may well be up, and delivery is the point.
    let queue = match DataDir::resolve(data_dir) {
        Ok(data) => Some(spool::Queue::new(&data)),
        Err(error) => {
            eprintln!("anamnesis: no queue for undelivered events: {error}");
            None
        }
    };

    // These budgets are deliberately tight. A hook runs before every tool call
    // an agent makes, so any delay here is multiplied by hundreds within one
    // session — and the case that matters is the server being *down*, where a
    // generous timeout turns "memory is not running" into "the agent feels
    // broken". Losing an event costs a line in a summary; stalling the session
    // costs the user's afternoon.
    let client = match reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(250))
        .timeout(std::time::Duration::from_millis(1_000))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("anamnesis: {error}");
            return;
        }
    };

    // Whatever earlier hooks could not deliver goes first, because a session is
    // a sequence and its start has to reach the index before its middle. On a
    // healthy machine — which is every hook of every session that never lost
    // the server — this costs one read of a directory that is not there.
    if let Some(queue) = &queue
        && !queue.is_empty()
    {
        replay(&client, server, token, queue);
    }

    let mut post = client
        .post(format!("{server}/hook"))
        .query(&[("agent", agent), ("event", delivery.as_str())])
        .header("content-type", "application/json")
        .body(payload.clone());
    if let Some(token) = token {
        post = post.bearer_auth(token);
    }
    let post = post.send();

    match post {
        Err(error) => {
            eprintln!("anamnesis: could not reach {server}: {error}");
            let kept = keep(queue.as_ref(), agent, &delivery, &payload);
            announce(
                agent,
                starting,
                &capture_notice(server, "could not be reached", kept),
            );
            return;
        }
        // Kept, unless the server has judged the payload itself. The common
        // refusal is not that — it is a token that does not match, which is
        // fixed in a minute and after which everything behind it should still
        // be there. A stuck queue is visible in `anamnesis status`; a dropped
        // event is visible nowhere, and that is the trade this queue exists to
        // make. The trade only holds while waiting can change the answer:
        // 400 and 413 are the server saying it read this one and will not
        // take it, however long it waits.
        Ok(response) if !response.status().is_success() => {
            let status = response.status();
            let refused = classify(status.as_u16()) == Delivery::Refused;
            let detail = response.text().unwrap_or_default();
            eprintln!(
                "anamnesis: server rejected event ({status}): {}",
                detail.trim()
            );
            // An event the server has already answered about does not go into
            // the line: it would be offered again, refused again, and stop
            // everything behind it in the meantime. It is kept out of the line
            // instead, because a refusal is not proof the event was bad — a
            // server older than the configuration it reads refuses good ones —
            // and `kept` stays false because nothing will deliver this one on
            // its own.
            let kept = if refused {
                set_aside(queue.as_ref(), agent, &delivery, &payload);
                false
            } else {
                keep(queue.as_ref(), agent, &delivery, &payload)
            };
            announce(
                agent,
                starting,
                &capture_notice(server, &format!("refused the event ({status})"), kept),
            );
            return;
        }
        Ok(_) => {}
    }

    // Only a starting session has anything to collect, and whatever comes back
    // goes to stdout, where the harness injects it into the model's context.
    //
    // Gemini CLI is the exception that shapes this: it requires stdout to be a
    // single JSON object and nothing else, so every event it fires gets one,
    // empty when there is nothing to say. Printing plain text there would not
    // fail loudly — it would fail as a parse error inside the harness, which
    // is the kind of failure this system is worst at explaining.
    if starting {
        let (session_id, cwd) = session_and_cwd(&payload);
        let mut request = client.get(format!("{server}/handoff")).query(&[
            ("agent", agent),
            ("session_id", &session_id),
            ("cwd", &cwd),
        ]);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        // The status is checked before the body is read, because this body
        // goes to stdout and the harness injects stdout into the model's
        // context. An error page printed as a handoff would not fail — it
        // would be believed.
        match request.send() {
            Ok(response) if response.status().is_success() => match response.text() {
                Ok(text) => print!("{}", handoff_reply(agent, &text)),
                Err(error) => {
                    eprintln!("anamnesis: handoff unavailable: {error}");
                    announce(
                        agent,
                        starting,
                        &handoff_notice("its reply could not be read"),
                    );
                }
            },
            Ok(response) => {
                let status = response.status();
                let detail = response.text().unwrap_or_default();
                eprintln!("anamnesis: handoff refused ({status}): {}", detail.trim());
                announce(
                    agent,
                    starting,
                    &handoff_notice(&format!("the server refused it ({status})")),
                );
            }
            Err(error) => {
                eprintln!("anamnesis: handoff unavailable: {error}");
                announce(
                    agent,
                    starting,
                    &handoff_notice("the server could not be reached"),
                );
            }
        }
    } else {
        // Every other event. Says nothing, except to the harness that wants an
        // object per call.
        announce(agent, false, "");
    }
}

/// Whether this event is the one that collects a handoff.
///
/// Three harnesses call it `SessionStart` and Cursor calls it `sessionStart`,
/// which is the whole of the difference — so it is compared without regard to
/// case rather than through a table. A harness that renames the moment
/// outright would need one, and this is where it would go.
fn is_starting(event: Option<&str>, _agent: &str) -> bool {
    event.is_some_and(|event| event.eq_ignore_ascii_case("sessionstart"))
}

/// How long one hook will spend delivering what earlier hooks could not.
///
/// The same reasoning as the timeouts in [`cmd_hook`], applied to a queue that
/// can hold days of work: draining it in one go would spend a session's budget
/// many times over. Two seconds moves a long outage in the background of the
/// next few dozen tool calls, which is roughly the pace it filled at.
const REPLAY_BUDGET: std::time::Duration = std::time::Duration::from_millis(2_000);

/// How many waiting events one read of the queue directory looks at.
const REPLAY_BATCH: usize = 32;

/// What one delivery attempt says about the event it carried.
///
/// The line the queue turns on is not success against failure but *whose*
/// failure it is. A server that is down, restarting, or holding a token
/// somebody is about to fix will take this event later, so it waits its turn.
/// A server that read the payload and answered about it will not take this
/// copy, and one of those at the head of an ordered queue stops every event
/// behind it for as long as the queue exists. That is capture ending quietly,
/// which is the failure the queue was written to prevent — so the refused
/// event leaves the line, and is kept beside it rather than thrown away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// It landed.
    Accepted,
    /// It did not land and it never will: the server judged this payload.
    Refused,
    /// It did not land this time.
    Failed,
}

/// Read a status the way the queue needs it read.
///
/// The split is the one `anamnesis-llm` already makes about its own providers:
/// a 429 or a 500 is a moment, a 400 or a 413 is an answer about this payload.
/// Credentials and addresses are moments too — a token that does not match yet
/// is fixed in a minute, and everything queued behind it should still be there
/// afterwards.
///
/// "Answered about" is deliberately weaker than "wrong". A server older than
/// the marker file it reads answers 400 to events that are perfectly good, as
/// this repository's own server did on 2026-09-01, and a restart accepts every
/// one of them. That is why a refusal moves the event aside instead of
/// deleting it: the queue has to keep moving, and the evidence has to survive
/// long enough for somebody to look at it.
fn classify(status: u16) -> Delivery {
    match status {
        200..=299 => Delivery::Accepted,
        400 | 413 | 415 | 422 => Delivery::Refused,
        _ => Delivery::Failed,
    }
}

/// What one pass over the queue did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Drained {
    /// Events the server took.
    delivered: usize,
    /// Events the server refused, now waiting in `pending/refused/`.
    set_aside: usize,
}

/// Deliver what earlier hooks could not, oldest first, stopping at the first
/// event the server could not take *yet*.
///
/// Stopping rather than skipping is the point: a session is a sequence, and
/// replaying its middle before its beginning would leave the index with a
/// session it cannot make sense of. A queue that will not drain shows up in
/// `anamnesis status`; dropping the event at the head to keep things moving
/// would be invisible, and invisible loss is what this queue was written to
/// end.
///
/// The exception is the event the server has already read and refused, which
/// waiting cannot help. Holding one of those keeps the position it lost and
/// costs every event behind it, so it is dropped — out loud, with the status
/// that judged it.
fn replay(
    client: &reqwest::blocking::Client,
    server: &str,
    token: Option<&str>,
    queue: &spool::Queue,
) {
    let outcome = drain(queue, REPLAY_BUDGET, |entry| {
        let Ok(body) = serde_json::to_string(&entry.body) else {
            // Written as JSON, read back as something else: nothing will ever
            // deliver this, and holding it would hold the queue.
            eprintln!("anamnesis: setting aside a waiting event that is no longer readable");
            return Delivery::Refused;
        };
        let mut post = client
            .post(format!("{server}/hook"))
            .query(&[("agent", entry.agent.as_str())])
            .header("content-type", "application/json")
            .body(body);
        // Under the name it was first offered under, where it has one: this is
        // the attempt most likely to be a repeat of one that arrived.
        if let Some(event) = &entry.event {
            post = post.query(&[("event", event.as_str())]);
        }
        if let Some(token) = token {
            post = post.bearer_auth(token);
        }
        let Ok(response) = post.send() else {
            return Delivery::Failed;
        };
        let status = response.status();
        let verdict = classify(status.as_u16());
        if verdict == Delivery::Refused {
            let detail = response.text().unwrap_or_default();
            eprintln!(
                "anamnesis: setting aside an event the server refused ({status}): {}",
                detail.trim()
            );
        }
        verdict
    });

    // stderr, never stdout: stdout is the handoff channel, and one harness
    // parses every byte of it as a single JSON object.
    if outcome.delivered > 0 {
        eprintln!(
            "anamnesis: delivered {} event(s) that had been waiting",
            outcome.delivered
        );
    }
    if outcome.set_aside > 0 {
        eprintln!(
            "anamnesis: set {} event(s) the server refused aside in {}",
            outcome.set_aside,
            queue.refused_root().display()
        );
    }
}

/// The order and the stopping, without the HTTP.
///
/// Split out for the same reason `finish_in_flight` is: the interesting half
/// is what happens when delivery starts failing partway through, and a live
/// server is a poor thing to write that test around. Returns how many went.
fn drain(
    queue: &spool::Queue,
    budget: std::time::Duration,
    mut deliver: impl FnMut(&spool::Queued) -> Delivery,
) -> Drained {
    let started = std::time::Instant::now();
    let mut outcome = Drained::default();

    'draining: while started.elapsed() < budget {
        let batch = queue.take(REPLAY_BATCH);
        if batch.is_empty() {
            break;
        }

        for (path, entry) in batch {
            if started.elapsed() >= budget {
                break 'draining;
            }
            match deliver(&entry) {
                Delivery::Accepted => {
                    queue.remove(&path);
                    outcome.delivered += 1;
                }
                // Moved rather than stepped over: skipping it would leave it
                // at the head to be offered, and refused, by every hook of
                // every session from now on. Order still holds for everything
                // that can still be delivered, and the event itself is still
                // on disk for whoever looks into why it was refused.
                Delivery::Refused => {
                    if let Err(error) = queue.set_aside(&path) {
                        eprintln!("anamnesis: could not set a refused event aside: {error}");
                        break 'draining;
                    }
                    outcome.set_aside += 1;
                }
                Delivery::Failed => break 'draining,
            }
        }
    }

    outcome
}

/// Keep an event the server did not take, and say whether it was kept.
///
/// The answer decides which sentence the session is told, so it has to be what
/// actually happened rather than what was attempted: a queue that refused the
/// event because it is full must not produce a notice promising delivery.
/// Keep an event the server has already refused, out of the line.
///
/// Through the queue, so the payload is redacted by the same rules before it
/// reaches the disk, and then straight out of it: this event is not waiting
/// for a retry, it is waiting for a person.
fn set_aside(queue: Option<&spool::Queue>, agent: &str, event: &str, payload: &str) -> bool {
    let Some(queue) = queue else {
        return false;
    };
    let kept = queue
        .push(agent, event, payload)
        .and_then(|path| Ok(queue.set_aside(&path)?));
    match kept {
        Ok(path) => {
            eprintln!(
                "anamnesis: the refused event was set aside in {}",
                path.display()
            );
            true
        }
        Err(error) => {
            eprintln!("anamnesis: could not set the refused event aside: {error}");
            false
        }
    }
}

fn keep(queue: Option<&spool::Queue>, agent: &str, event: &str, payload: &str) -> bool {
    let Some(queue) = queue else {
        return false;
    };
    match queue.push(agent, event, payload) {
        Ok(_) => true,
        Err(error) => {
            eprintln!("anamnesis: could not keep the event: {error}");
            false
        }
    }
}

/// Say something on stdout, in the shape this harness reads back.
///
/// Every path out of the hook goes through here, because stdout is a contract
/// and the contract does not lapse when something has gone wrong. Gemini CLI
/// parses stdout as one JSON object on **every** event it fires; before this,
/// a failed POST returned early and printed nothing at all, so the harness
/// least able to survive silence got silence exactly when the server was down.
///
/// Only a starting session carries a message. A notice on every tool call
/// would be the same sentence a hundred times in one context window, and the
/// place to learn that capture is down is the top of the session, once.
fn announce(agent: &str, starting: bool, text: &str) {
    if starting {
        print!("{}", handoff_reply(agent, text));
    } else if agent == "gemini-cli" {
        print!("{}", handoff_reply(agent, ""));
    }
}

/// What the model is told when the event it triggered never reached memory.
///
/// This exists because of how the failure looked from the outside: the server
/// was not running for four days, every hook in every session failed to
/// connect, and nothing anyone would see said so. The hook writes to stderr
/// and exits zero — deliberately, since a hook that fails loudly at the shell
/// level would interrupt hundreds of tool calls — and no harness surfaces
/// that. `anamnesis status` says it plainly, but only to someone who already
/// suspects.
///
/// stdout is the one channel a harness is guaranteed to read: it is how the
/// handoff reaches the model. So the notice goes there, at session start,
/// where whoever is working can be told in the same breath as the memory they
/// asked for.
///
/// It names itself. The standing rule is that a server's *response body* must
/// never be printed here — an error page injected as context would not fail,
/// it would be believed — and the way this stays on the right side of that
/// rule is that anamnesis wrote it, says so, and says what to do about it.
///
/// Two sentences, because since the queue there are two different facts. An
/// event that was kept is not an event that was lost, and telling someone
/// their afternoon is going unrecorded when it is waiting on disk would be its
/// own false alarm — the same mistake in the other direction as [`handoff_notice`].
fn capture_notice(server: &str, reason: &str, kept: bool) -> String {
    if kept {
        format!(
            "[anamnesis] The memory server at {server} {reason}, so nothing here is reaching \
             memory yet — the events are being kept and will be delivered when it is running. \
             `anamnesis status` says why, `anamnesis serve` starts it."
        )
    } else {
        format!(
            "[anamnesis] This session is NOT being recorded: the memory server at {server} \
             {reason}. Nothing said here will be remembered until it is running — `anamnesis \
             status` says why, `anamnesis serve` starts it."
        )
    }
}

/// What the model is told when the event was recorded but the handoff was not.
///
/// Deliberately not the same sentence. Capture is working here, and saying
/// otherwise would send someone to restart a server that is already up. The
/// thing worth knowing is narrower and easy to misread: an empty handoff and a
/// handoff that could not be fetched look identical to a model, and one of them
/// means the last session left notes that are still waiting.
fn handoff_notice(reason: &str) -> String {
    format!(
        "[anamnesis] The previous session's handoff could not be collected: {reason}. This \
         session is starting without it and there may be notes still waiting; capture itself is \
         working."
    )
}

/// The handoff, in the shape the harness reads back.
///
/// Claude Code and Codex inject whatever a hook prints, so the handoff is
/// printed as it is and nothing is printed when there is none. Gemini CLI
/// parses stdout as one JSON object and rejects anything else, so it gets one
/// — carrying the handoff as `hookSpecificOutput.additionalContext`, or empty
/// when there is nothing to hand over.
///
/// The trailing newline matters only for the plain form, where stdout is
/// spliced into a prompt.
fn handoff_reply(agent: &str, handoff: &str) -> String {
    let handoff = handoff.trim();

    if agent == "gemini-cli" {
        let reply = if handoff.is_empty() {
            serde_json::json!({})
        } else {
            serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "SessionStart",
                    "additionalContext": handoff,
                }
            })
        };
        return format!("{reply}\n");
    }

    // Cursor takes injected context as a top-level `additional_context`, and
    // unlike Gemini it does not insist on being spoken to: nothing to hand
    // over means nothing printed.
    if agent == "cursor" {
        if handoff.is_empty() {
            return String::new();
        }
        return format!("{}\n", serde_json::json!({ "additional_context": handoff }));
    }

    if handoff.is_empty() {
        return String::new();
    }
    format!("{handoff}\n")
}

/// Pull the session id and working directory out of a hook payload.
fn session_and_cwd(payload: &str) -> (String, String) {
    let value: serde_json::Value = serde_json::from_str(payload).unwrap_or_default();
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    let cwd = match field("cwd") {
        empty if empty.is_empty() => std::env::current_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        found => found,
    };
    (field("session_id"), cwd)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued() -> (tempfile::TempDir, spool::Queue) {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = DataDir::resolve(Some(dir.path().to_path_buf())).expect("data dir");
        let queue = spool::Queue::new(&data);
        (dir, queue)
    }

    /// The failure this whole notice exists for: the server was down for four
    /// days, every hook failed to connect, and nothing anyone would read said
    /// so. Whatever else it says, it has to say that nothing is being kept.
    #[test]
    fn the_capture_notice_says_that_nothing_is_being_recorded() {
        let notice = capture_notice("http://127.0.0.1:8080", "could not be reached", false);

        assert!(notice.contains("NOT being recorded"), "{notice}");
        assert!(notice.contains("http://127.0.0.1:8080"), "{notice}");
        assert!(notice.contains("anamnesis serve"), "{notice}");
        assert!(
            !notice.contains('\n'),
            "one line, not a paragraph: {notice}"
        );
    }

    /// The four days this queue exists for: the server was down, every hook
    /// failed to connect, and every event went on the floor.
    #[test]
    fn an_event_the_server_would_not_take_is_kept() {
        let (_dir, queue) = queued();

        let kept = keep(
            Some(&queue),
            "claude-code",
            "01998f3a-0000-7000-8000-00000000feed",
            r#"{"hook_event_name":"UserPromptSubmit"}"#,
        );

        assert!(kept);
        assert_eq!(queue.len(), 1);
    }

    /// The notice has to describe what happened, not what was attempted. A
    /// queue that refused the event must not produce a sentence promising it
    /// will be delivered later.
    #[test]
    fn an_event_the_queue_refused_is_reported_as_lost() {
        let (_dir, queue) = queued();

        let kept = keep(
            Some(&queue),
            "claude-code",
            "01998f3a-0000-7000-8000-00000000dead",
            "not json at all",
        );

        assert!(!kept);
        assert!(queue.is_empty());
        assert!(
            capture_notice("http://x", "could not be reached", kept).contains("NOT being recorded")
        );
    }

    /// And the other direction, which is the one the queue makes possible:
    /// telling someone their afternoon is unrecorded when it is waiting on
    /// disk would be the notice's own false alarm.
    #[test]
    fn a_kept_event_is_not_reported_as_lost() {
        let notice = capture_notice("http://127.0.0.1:8080", "could not be reached", true);

        assert!(!notice.contains("NOT being recorded"), "{notice}");
        assert!(notice.contains("kept"), "{notice}");
        assert!(notice.contains("anamnesis serve"), "{notice}");
        assert!(notice.starts_with("[anamnesis]"), "{notice}");
        assert!(!notice.contains('\n'), "one line: {notice}");
    }

    /// A session is a sequence. Delivering what is behind a refusal would put
    /// its middle in the index ahead of its beginning, so the replay stops
    /// where it fails and leaves the rest in order behind it.
    #[test]
    fn waiting_events_replay_oldest_first_and_stop_at_the_first_refusal() {
        let (_dir, queue) = queued();
        for n in 0..5 {
            queue
                .push(
                    "claude-code",
                    &format!("event-{n}"),
                    &format!(r#"{{"n":{n}}}"#),
                )
                .expect("queued");
        }

        // The third will not go: a server that came back and went away again,
        // which is what a restarting one looks like from here.
        let mut seen = Vec::new();
        let outcome = drain(&queue, std::time::Duration::from_secs(5), |entry| {
            let n = entry.body["n"].as_i64().expect("n");
            if n == 2 {
                return Delivery::Failed;
            }
            seen.push(n);
            Delivery::Accepted
        });

        assert_eq!(outcome.delivered, 2);
        assert_eq!(outcome.set_aside, 0);
        assert_eq!(seen, [0, 1], "oldest first");
        let left: Vec<i64> = queue
            .take(10)
            .iter()
            .map(|(_, entry)| entry.body["n"].as_i64().expect("n"))
            .collect();
        assert_eq!(
            left,
            [2, 3, 4],
            "the refused event stays at the head and nothing behind it was skipped"
        );
    }

    /// The other kind of refusal, and the one that used to end capture without
    /// saying so: an event the server has read and answered about. Offering it
    /// again gets the same answer, so holding it at the head would cost every
    /// event behind it — for as long as the queue exists. It leaves the line
    /// and is kept beside it, because a refusal is not proof the event was
    /// bad.
    #[test]
    fn an_event_the_server_refused_leaves_the_line_and_the_rest_go() {
        let (_dir, queue) = queued();
        for n in 0..5 {
            queue
                .push(
                    "claude-code",
                    &format!("event-{n}"),
                    &format!(r#"{{"n":{n}}}"#),
                )
                .expect("queued");
        }

        let mut seen = Vec::new();
        let outcome = drain(&queue, std::time::Duration::from_secs(5), |entry| {
            let n = entry.body["n"].as_i64().expect("n");
            if n == 2 {
                return Delivery::Refused;
            }
            seen.push(n);
            Delivery::Accepted
        });

        assert_eq!(outcome.delivered, 4);
        assert_eq!(outcome.set_aside, 1);
        assert_eq!(seen, [0, 1, 3, 4], "order held around the refused event");
        assert!(
            queue.is_empty(),
            "the refused event was left to block the queue again"
        );
        assert_eq!(
            queue.set_aside_len(),
            1,
            "the refused event was thrown away rather than kept"
        );
    }

    /// And it is still readable where it was put. A server older than the
    /// marker file it reads refuses good events — this repository's own did on
    /// 2026-09-01 — so what was refused has to survive long enough for
    /// somebody to look at it.
    #[test]
    fn a_refused_event_is_still_there_to_be_read() {
        let (_dir, queue) = queued();
        queue
            .push(
                "claude-code",
                "event-0",
                r#"{"prompt":"the one that failed"}"#,
            )
            .expect("queued");

        drain(&queue, std::time::Duration::from_secs(5), |_| {
            Delivery::Refused
        });

        let refused = std::fs::read_dir(queue.refused_root())
            .expect("the refused directory")
            .flatten()
            .map(|entry| std::fs::read_to_string(entry.path()).expect("read"))
            .collect::<Vec<_>>();

        assert_eq!(refused.len(), 1);
        assert!(refused[0].contains("the one that failed"), "{:?}", refused);
    }

    /// Which failures are the server's answer about this payload, and which
    /// are the moment. Read wrongly in either direction it costs something: an
    /// answer treated as a moment stops the queue forever, and a moment
    /// treated as an answer takes an event out of a line a restart would have
    /// drained.
    #[test]
    fn an_answer_about_the_payload_is_told_apart_from_a_bad_moment() {
        assert_eq!(classify(200), Delivery::Accepted);
        assert_eq!(classify(202), Delivery::Accepted);

        for status in [400, 413, 415, 422] {
            assert_eq!(classify(status), Delivery::Refused, "{status}");
        }

        // A token that does not match yet, an address that is wrong yet, a
        // server that is busy or restarting: all fixed without touching the
        // event.
        for status in [401, 403, 404, 405, 429, 500, 502, 503] {
            assert_eq!(classify(status), Delivery::Failed, "{status}");
        }
    }

    /// It is injected where a handoff goes, so it has to be impossible to read
    /// as one. The standing rule is that a server's response body is never
    /// printed there — this stays on the right side of it by being written
    /// here, and saying whose words they are.
    #[test]
    fn a_notice_names_itself_rather_than_passing_as_memory() {
        assert!(
            capture_notice("http://x", "could not be reached", false).starts_with("[anamnesis]")
        );
        assert!(handoff_notice("the server could not be reached").starts_with("[anamnesis]"));
    }

    /// Two failures that need different sentences. Telling someone capture is
    /// dead when only the handoff failed sends them to restart a server that
    /// is already running, and the notice would be its own false alarm.
    #[test]
    fn a_failed_handoff_does_not_claim_capture_is_broken() {
        let notice = handoff_notice("the server refused it (401 Unauthorized)");

        assert!(notice.contains("401 Unauthorized"), "{notice}");
        assert!(notice.contains("capture itself is working"), "{notice}");
        assert!(!notice.contains("NOT being recorded"), "{notice}");
    }

    /// A notice is delivered the same way a handoff is, so it survives every
    /// harness's idea of stdout — including the one that parses it as JSON.
    #[test]
    fn a_notice_reaches_every_harness_in_its_own_shape() {
        let notice = capture_notice("http://127.0.0.1:8080", "could not be reached", false);

        let gemini: serde_json::Value =
            serde_json::from_str(&handoff_reply("gemini-cli", &notice)).expect("valid JSON");
        assert!(
            gemini["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .expect("context")
                .contains("NOT being recorded")
        );

        let cursor: serde_json::Value =
            serde_json::from_str(&handoff_reply("cursor", &notice)).expect("valid JSON");
        assert!(
            cursor["additional_context"]
                .as_str()
                .expect("context")
                .contains("NOT being recorded")
        );

        assert!(handoff_reply("claude-code", &notice).starts_with("[anamnesis]"));
    }

    /// The harness that shapes this: Gemini CLI parses stdout as one JSON
    /// object and rejects anything else, so plain text there is not a
    /// degraded handoff — it is a parse error inside somebody's agent.
    #[test]
    fn gemini_gets_one_json_object_whether_or_not_there_is_a_handoff() {
        let with = handoff_reply("gemini-cli", "Last request: wire it up\n");
        let parsed: serde_json::Value = serde_json::from_str(&with).expect("valid JSON");
        assert_eq!(
            parsed["hookSpecificOutput"]["additionalContext"],
            "Last request: wire it up"
        );
        assert_eq!(
            parsed["hookSpecificOutput"]["hookEventName"],
            "SessionStart"
        );

        let without: serde_json::Value =
            serde_json::from_str(&handoff_reply("gemini-cli", "")).expect("valid JSON");
        assert_eq!(without, serde_json::json!({}));
    }

    /// Everything else injects whatever the hook printed, so the handoff is
    /// printed as it is — and nothing at all when there is none, because an
    /// empty line is still a line in somebody's context.
    #[test]
    fn the_other_harnesses_get_the_handoff_as_it_is() {
        assert_eq!(
            handoff_reply("claude-code", "Last request: wire it up"),
            "Last request: wire it up\n"
        );
        assert_eq!(handoff_reply("codex", "  "), "");
        assert_eq!(handoff_reply("claude-code", ""), "");
    }

    /// A refused or unreachable handoff must not leave Gemini with an empty
    /// stdout it was told never to expect.
    #[test]
    fn a_failed_handoff_still_leaves_gemini_a_valid_object() {
        let reply = handoff_reply("gemini-cli", "");
        serde_json::from_str::<serde_json::Value>(&reply).expect("valid JSON");
    }

    /// Cursor takes context back as a top-level `additional_context`, and
    /// unlike Gemini it is content with silence when there is none.
    #[test]
    fn cursor_gets_its_own_field_and_nothing_when_there_is_nothing() {
        let reply = handoff_reply("cursor", "Last request: wire it up");
        let parsed: serde_json::Value = serde_json::from_str(&reply).expect("valid JSON");
        assert_eq!(parsed["additional_context"], "Last request: wire it up");
        assert_eq!(handoff_reply("cursor", ""), "");
    }

    #[test]
    fn only_a_starting_session_collects_a_handoff() {
        assert!(is_starting(Some("SessionStart"), "claude-code"));
        assert!(!is_starting(Some("SessionEnd"), "claude-code"));
        assert!(!is_starting(None, "gemini-cli"));
        // Cursor spells it differently, and it is still the same moment.
        assert!(is_starting(Some("sessionStart"), "cursor"));
    }
}
