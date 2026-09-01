//! What `anamnesis status` answers, and how it decides.
//!
//! One question — is my work being recorded right now — and it takes three
//! facts to answer honestly: whether the server is reachable, when capture
//! last reached the index, and what memory currently holds. A server that
//! answers `/health` proves only that something is listening; hooks that were
//! never installed look exactly like a quiet afternoon; and a token this
//! machine does not have looks exactly like a server that is down. Every
//! sentence in this module exists because two different problems would
//! otherwise print the same line.
//!
//! Nothing here writes anything. It probes, reads, and phrases.

use std::path::PathBuf;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::Store;
use jiff::Timestamp;

use crate::format::{describe_age, describe_source, plural};
use crate::spool;

pub fn cmd_status(
    verbose: bool,
    server: &str,
    token: Option<&str>,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let scope = resolve_scope(&cwd)?;
    let data = DataDir::resolve(data_dir)?;
    let queue = spool::Queue::new(&data);

    println!("📚 Anamnesis Memory Status");
    println!();
    println!("  Workspace: {}", scope.scope.workspace);
    println!("  Project:   {}", scope.scope.project);
    println!("  Identity:  {}", describe_source(&scope.source));
    if let Some(line) = describe_unrecognized(&scope.unrecognized) {
        println!("  Marker:    {line}");
    }

    if verbose {
        println!();
        println!("  Project key:  {}", scope.key);
        println!("  Project id:   {}", scope.project_id);
        println!("  Workspace id: {}", scope.workspace_id);
        println!("  Data dir:     {}", data.root().display());
        println!(
            "  Wiki:         {}",
            data.wiki_scope(&scope.scope).display()
        );
        println!("  Index:        {}", data.db_file().display());
        println!("  Queue:        {}", queue.root().display());
        match &scope.marker {
            Some(path) => println!("  Marker:       {}", path.display()),
            None => println!("  Marker:       (none)"),
        }

        // This shell's environment, which is what a server started from
        // *here* would use — not what the running server was started with.
        // The `Summaries:` line above is the server's own answer, and the two
        // disagreeing is the normal case on a machine where the server is
        // launched by something else.
        match anamnesis_llm::LlmConfig::from_env() {
            Ok(llm) if llm.provider == anamnesis_llm::ProviderKind::None => {
                println!("  Model here:   (none — a server started here would count)");
            }
            Ok(llm) => println!("  Model here:   {}", llm.model),
            Err(error) => println!("  Model here:   misconfigured — {error}"),
        }
    }

    if !data.db_file().exists() {
        println!();
        println!("  Not initialized yet — run `anamnesis init`.");
        return Ok(());
    }

    // Migrated like every other read-only command opens it. A status that read
    // an older schema would be describing a different database from the one
    // `search` and `sessions` use, which is worse than taking a moment here.
    let store = Store::open(data.db_file())?;
    store.migrate()?;

    let now = Timestamp::now();
    let reachable = probe_server(server);
    let facts = probe_server_facts(server, token, &reachable);
    println!();
    println!("  Server:    {}", describe_server(server, &reachable));
    println!(
        "  Auth:      {}",
        describe_auth(&facts.auth, token.is_some())
    );
    // Asked of the server rather than of this shell, and printed here rather
    // than behind `--verbose`, because "every summary is a word count" is not
    // a detail — it is the difference between a memory and a log, and the only
    // other place that said so was a banner nobody sees.
    if let Some(line) = describe_consolidation(&facts.consolidation) {
        println!("  Summaries: {line}");
    }
    if let Some(line) = describe_embedding(&facts.embedding) {
        println!("  Vectors:   {line}");
    }
    println!(
        "  Capture:   {}",
        describe_capture(
            store.last_observation_at(scope.project_id)?,
            now,
            queue.len(),
            queue.set_aside_len()
        )
    );
    // Where a project keeps a slot per operator, the handoff reported has to
    // be the one *this* machine would be handed. Reporting the shared slot
    // would tell an operator with a note waiting that nothing is waiting.
    let operator = if scope.slots.per_user {
        facts.auth.operator()
    } else {
        None
    };
    let slot = anamnesis_core::handoff::Slot::shared().for_operator(operator.clone());
    println!(
        "  Memory:    {}",
        describe_memory(
            store.session_count(scope.project_id)?,
            store.page_count(scope.project_id)?,
            store.peek_handoff(scope.project_id, &slot)?.is_some(),
            operator.as_ref(),
        )
    );

    Ok(())
}

/// Whether the process that records events is answering.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerState {
    /// Reachable, and answering as anamnesis does.
    Running,
    /// Something is listening, but it is not answering `/health`.
    Foreign(u16),
    /// Nothing answered.
    Down,
}

/// Ask the server whether it is there.
///
/// The budget is deliberately more generous than the hook's one second. A hook
/// that waits is a stutter in someone's editing session, so it gives up fast;
/// here a person has asked and is watching the terminal, and calling a
/// slow-but-running server dead is the more expensive mistake of the two.
fn probe_server(server: &str) -> ServerState {
    let Ok(client) = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return ServerState::Down;
    };

    match client.get(format!("{server}/health")).send() {
        Ok(response) if response.status().is_success() => ServerState::Running,
        Ok(response) => ServerState::Foreign(response.status().as_u16()),
        Err(_) => ServerState::Down,
    }
}

/// One line for where events are being delivered, and whether anyone is there.
///
/// The address is always named. Someone running the server on another port has
/// not lost their memory, and a line that said only "unreachable" would send
/// them looking for the wrong problem.
fn describe_server(server: &str, state: &ServerState) -> String {
    match state {
        ServerState::Running => format!("running at {server}"),
        ServerState::Foreign(code) => {
            format!("something else answered {code} at {server}")
        }
        ServerState::Down => {
            format!("not running at {server} — nothing is being captured")
        }
    }
}

/// What the server makes of this machine's token.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum AuthState {
    /// The server did not answer the question, so nothing is claimed.
    #[default]
    Unknown,
    /// No token is required. Every caller is accepted.
    Open,
    /// A token was accepted, naming this operator when it named one.
    Accepted(Option<String>),
    /// A token is required and this machine's was not one of them.
    Rejected,
}

impl AuthState {
    /// The operator the server named, when it named one this crate can use.
    ///
    /// A name that will not parse is treated as no name: it can only come from
    /// a server newer than this client, and guessing at it would key a slot to
    /// something that is not an operator.
    fn operator(&self) -> Option<anamnesis_core::scope::OperatorName> {
        match self {
            Self::Accepted(Some(name)) => anamnesis_core::scope::OperatorName::parse(name).ok(),
            _ => None,
        }
    }
}

/// Ask the server what it makes of this machine's token.
///
/// `/whoami` and not `/handoff`, because the question has to be asked without
/// changing anything: collecting a handoff to find out whether a token works
/// would spend the handoff.
fn probe_server_facts(server: &str, token: Option<&str>, reachable: &ServerState) -> ServerFacts {
    if *reachable != ServerState::Running {
        return ServerFacts::default();
    }

    let Ok(client) = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return ServerFacts::default();
    };

    let mut request = client.get(format!("{server}/whoami"));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    match request.send() {
        Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
            ServerFacts::from(AuthState::Rejected)
        }
        Ok(response) if response.status().is_success() => {
            let body = response.json::<serde_json::Value>().unwrap_or_default();
            let auth = if body.get("auth").and_then(|v| v.as_str()) == Some("open") {
                AuthState::Open
            } else {
                AuthState::Accepted(
                    body.get("operator")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
                )
            };
            ServerFacts {
                auth,
                consolidation: ServerModel::read(&body, "consolidation"),
                embedding: ServerModel::read(&body, "embedding"),
            }
        }
        // Anything else — including the 404 an older server returns — is a
        // question this server did not answer, and guessing at it would be
        // worse than saying so.
        Ok(_) | Err(_) => ServerFacts::default(),
    }
}

/// One model a server reported, or did not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum ServerModel {
    /// The server never mentioned it, which means it is older than this
    /// client. Saying "no model" here would be a confident lie about the
    /// thing this whole report exists to make visible.
    #[default]
    Unstated,
    /// It said, explicitly, that it has none.
    Absent,
    /// The model it named.
    Named(String),
}

impl ServerModel {
    /// Read one field of a `/whoami` body, keeping "absent" and "not
    /// mentioned" apart.
    fn read(body: &serde_json::Value, field: &str) -> Self {
        match body.get(field) {
            None => Self::Unstated,
            Some(serde_json::Value::String(name)) => Self::Named(name.clone()),
            Some(_) => Self::Absent,
        }
    }
}

/// What a server said about itself when `status` asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ServerFacts {
    /// What it made of this machine's token.
    auth: AuthState,
    /// The model it summarises sessions with.
    consolidation: ServerModel,
    /// The model it embeds pages with.
    embedding: ServerModel,
}

impl From<AuthState> for ServerFacts {
    fn from(auth: AuthState) -> Self {
        Self {
            auth,
            ..Self::default()
        }
    }
}

/// How the server turns sessions into pages.
fn describe_consolidation(model: &ServerModel) -> Option<String> {
    match model {
        ServerModel::Unstated => None,
        ServerModel::Absent => Some(
            "counted — the server has no model, so pages carry facts and no reading of them"
                .to_owned(),
        ),
        ServerModel::Named(model) => Some(format!("written by {model}")),
    }
}

/// Whether the server writes vectors for what it indexes.
fn describe_embedding(model: &ServerModel) -> Option<String> {
    match model {
        ServerModel::Unstated => None,
        ServerModel::Absent => Some(format!(
            "off — retrieval runs without them (set {} on the server)",
            "ANAMNESIS_EMBED_ENABLED=1"
        )),
        ServerModel::Named(model) => Some(model.clone()),
    }
}

/// One line for whether this memory is protected, and whether this machine
/// gets in.
///
/// Both halves are needed. A server that requires tokens is no comfort if the
/// hooks on this machine are being turned away, and that failure is otherwise
/// invisible: rejected events look exactly like a quiet afternoon.
fn describe_auth(state: &AuthState, presented: bool) -> String {
    let token_env = anamnesis_web::auth::TOKEN_ENV;
    match state {
        AuthState::Unknown => "unknown — the server did not answer".to_owned(),
        AuthState::Open => {
            "not required — anything that can reach this port can read this memory".to_owned()
        }
        AuthState::Accepted(None) => "required — this client's token was accepted".to_owned(),
        AuthState::Accepted(Some(operator)) => {
            format!("required — this client is {operator}")
        }
        AuthState::Rejected if presented => {
            format!("required — this client's token was rejected; check {token_env}")
        }
        AuthState::Rejected => {
            format!("required — this client has no token; set {token_env}")
        }
    }
}

/// One line for the parts of the marker file this build did not apply.
///
/// Silent when there are none, which is every machine whose binary keeps up
/// with its marker file. When there are some, this is the only place they are
/// ever mentioned: the settings are in the file, a person can see them there,
/// and nothing else would ever say that the build reading them has no code for
/// them. The line names them and says what to do, because "upgrade" is the
/// whole fix.
fn describe_unrecognized(tables: &[String]) -> Option<String> {
    if tables.is_empty() {
        return None;
    }
    let named = tables
        .iter()
        .map(|table| format!("[{table}]"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "{named} {} not understood by this build and had no effect — upgrade anamnesis to apply {}",
        if tables.len() == 1 { "is" } else { "are" },
        if tables.len() == 1 { "it" } else { "them" }
    ))
}

/// One line for when capture last reached the index.
///
/// A reachable server proves nothing on its own: it records only what a
/// harness sends it, and hooks that were never installed look exactly like a
/// quiet afternoon. This is the half of the answer that comes from evidence.
fn describe_capture(
    last: Option<Timestamp>,
    now: Timestamp,
    waiting: usize,
    set_aside: usize,
) -> String {
    let recorded = match last {
        Some(at) => format!("last event {}", describe_age(at, now)),
        None => "nothing captured yet — run `anamnesis install-hooks`".to_owned(),
    };

    // Silent when the queue is empty, which is every healthy machine. A line
    // that is always there is a line nobody reads on the day it matters — the
    // same reason the drift report says nothing when the wiki and the index
    // agree.
    let line = match waiting {
        0 => recorded,
        1 => format!("{recorded} · 1 event waiting to be delivered"),
        n => format!("{recorded} · {n} events waiting to be delivered"),
    };

    // The other half of the same question, and the half that does not fix
    // itself: these were refused, they are out of the line so the rest could
    // go, and nothing will offer them again. Reporting them where somebody is
    // already asking "is my work being recorded" is the only thing standing
    // between a refused event and an afternoon nobody knows is missing.
    match set_aside {
        0 => line,
        1 => format!("{line} · 1 event the server refused, set aside"),
        n => format!("{line} · {n} events the server refused, set aside"),
    }
}

/// One line for what this project's memory currently holds.
fn describe_memory(
    sessions: i64,
    pages: i64,
    handoff: bool,
    operator: Option<&anamnesis_core::scope::OperatorName>,
) -> String {
    // Named only where a project keeps more than one slot: "no handoff
    // waiting" and "no handoff waiting for alice" are different facts, and on
    // a shared server the second is the one that stops someone concluding
    // their memory is empty.
    let whose = match operator {
        Some(operator) => format!(" for {operator}"),
        None => String::new(),
    };
    format!(
        "{} · {} · {}{whose}",
        plural(sessions, "session"),
        plural(pages, "page"),
        if handoff {
            "handoff waiting"
        } else {
            "no handoff waiting"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same three lines the handoff tests keep, kept twice rather than
    /// exported from a test module: a helper shared across `#[cfg(test)]`
    /// boundaries costs more to read than it saves.
    fn an_operator(name: &str) -> anamnesis_core::scope::OperatorName {
        anamnesis_core::scope::OperatorName::sanitized(name).expect("a usable operator name")
    }

    fn at(raw: &str) -> Timestamp {
        raw.parse().expect("timestamp")
    }

    /// The whole point of the line: an unreachable server means events are
    /// being dropped right now, and saying so is the only warning anyone gets.
    #[test]
    fn a_server_that_is_not_running_says_capture_has_stopped() {
        let line = describe_server("http://127.0.0.1:8080", &ServerState::Down);
        assert!(line.contains("nothing is being captured"), "{line}");
        assert!(line.contains("http://127.0.0.1:8080"), "{line}");
    }

    /// Naming the address is what separates "your memory is gone" from "you
    /// are looking at the wrong port".
    #[test]
    fn every_server_line_names_where_it_looked() {
        for state in [
            ServerState::Running,
            ServerState::Foreign(404),
            ServerState::Down,
        ] {
            let line = describe_server("http://example:9999", &state);
            assert!(line.contains("http://example:9999"), "{state:?}: {line}");
        }
    }

    /// A port answering something that is not anamnesis is a different problem
    /// from a port answering nothing, and needs a different fix.
    #[test]
    fn a_foreign_listener_is_not_reported_as_a_running_server() {
        let line = describe_server("http://127.0.0.1:8080", &ServerState::Foreign(404));
        assert!(!line.contains("running at"), "{line}");
        assert!(line.contains("404"), "{line}");
    }

    /// Hooks that were never installed look exactly like a quiet afternoon, so
    /// the empty case has to point at the thing that fixes it.
    #[test]
    fn capturing_nothing_yet_points_at_install_hooks() {
        let line = describe_capture(None, at("2026-08-25T12:00:00Z"), 0, 0);
        assert!(line.contains("install-hooks"), "{line}");
    }

    #[test]
    fn capture_reports_how_long_ago_the_last_event_landed() {
        let line = describe_capture(
            Some(at("2026-08-25T11:57:00Z")),
            at("2026-08-25T12:00:00Z"),
            0,
            0,
        );
        assert_eq!(line, "last event 3m ago");
    }

    #[test]
    fn a_waiting_handoff_is_reported_as_waiting() {
        assert!(describe_memory(2, 1, true, None).contains("handoff waiting"));
        assert!(describe_memory(2, 1, false, None).contains("no handoff waiting"));
    }

    /// The line exists to answer "is my work reaching memory", so the case
    /// where it is not — a hook whose token the server refuses — has to read
    /// as a problem and name what to check.
    #[test]
    fn a_rejected_token_says_which_variable_is_wrong() {
        let line = describe_auth(&AuthState::Rejected, true);
        assert!(line.contains("rejected"), "{line}");
        assert!(line.contains(anamnesis_web::auth::TOKEN_ENV), "{line}");
    }

    /// Missing and wrong are different fixes: one is "set the variable", the
    /// other is "you set it to the wrong thing".
    #[test]
    fn no_token_at_all_is_reported_differently_from_a_wrong_one() {
        let missing = describe_auth(&AuthState::Rejected, false);
        let wrong = describe_auth(&AuthState::Rejected, true);
        assert_ne!(missing, wrong);
        assert!(missing.contains("has no token"), "{missing}");
    }

    #[test]
    fn an_open_server_is_reported_as_open_rather_than_as_working() {
        let line = describe_auth(&AuthState::Open, false);
        assert!(line.contains("not required"), "{line}");
        assert!(line.contains("can read this memory"), "{line}");
    }

    #[test]
    fn an_accepted_token_names_the_operator_when_it_has_one() {
        assert!(
            describe_auth(&AuthState::Accepted(Some("alice".to_owned())), true).contains("alice")
        );
        assert!(describe_auth(&AuthState::Accepted(None), true).contains("accepted"));
    }

    /// The failure this line was added for: a memory whose every page was a
    /// word count, for a week, with nothing in reach saying so.
    #[test]
    fn a_server_with_no_model_says_its_summaries_are_counted() {
        let line = describe_consolidation(&ServerModel::Absent).expect("a line");
        assert!(line.contains("counted"), "{line}");
    }

    #[test]
    fn a_server_with_a_model_names_it() {
        let line = describe_consolidation(&ServerModel::Named("claude-opus-5".to_owned()))
            .expect("a line");
        assert!(line.contains("claude-opus-5"), "{line}");
        assert!(!line.contains("counted"), "{line}");
    }

    /// An older server does not answer the question. Reporting "counted" for
    /// it would be the same confident lie in the other direction.
    #[test]
    fn a_server_that_did_not_say_is_not_reported_as_having_no_model() {
        assert_eq!(describe_consolidation(&ServerModel::Unstated), None);
        assert_eq!(describe_embedding(&ServerModel::Unstated), None);
    }

    #[test]
    fn vectors_being_off_names_the_variable_that_turns_them_on() {
        let line = describe_embedding(&ServerModel::Absent).expect("a line");
        assert!(line.contains("ANAMNESIS_EMBED_ENABLED"), "{line}");
    }

    /// The three cases the wire has to keep apart: a name, an explicit
    /// nothing, and a field an older server never wrote.
    #[test]
    fn a_model_field_tells_absent_apart_from_unmentioned() {
        let named = serde_json::json!({"consolidation": "claude-opus-5"});
        let absent = serde_json::json!({"consolidation": null});
        let older = serde_json::json!({"auth": "open"});

        assert_eq!(
            ServerModel::read(&named, "consolidation"),
            ServerModel::Named("claude-opus-5".to_owned())
        );
        assert_eq!(
            ServerModel::read(&absent, "consolidation"),
            ServerModel::Absent
        );
        assert_eq!(
            ServerModel::read(&older, "consolidation"),
            ServerModel::Unstated
        );
    }

    /// A server that did not answer is not a server that is open. Saying
    /// "not required" here would be a false all-clear.
    #[test]
    fn a_silent_server_is_not_reported_as_unprotected() {
        let line = describe_auth(&AuthState::Unknown, false);
        assert!(line.contains("unknown"), "{line}");
        assert!(!line.contains("not required"), "{line}");
    }

    /// The name the server gave is the one the slot is keyed by, so a name
    /// this build could not use as a key must not be treated as one.
    #[test]
    fn an_unusable_operator_name_is_no_operator_at_all() {
        assert_eq!(
            AuthState::Accepted(Some("alice".to_owned())).operator(),
            Some(an_operator("alice"))
        );
        assert_eq!(
            AuthState::Accepted(Some("Not A Name".to_owned())).operator(),
            None
        );
        assert_eq!(AuthState::Accepted(None).operator(), None);
        assert_eq!(AuthState::Open.operator(), None);
    }

    /// On a shared server, "no handoff waiting" without a name is the sentence
    /// that makes someone believe their memory is empty when it is somebody
    /// else's slot they are looking at.
    #[test]
    fn the_memory_line_names_the_slot_it_looked_in() {
        let alice = an_operator("alice");
        let line = describe_memory(2, 1, false, Some(&alice));
        assert!(line.contains("no handoff waiting for alice"), "{line}");

        // And says nothing extra where there is only one slot to look in.
        let shared = describe_memory(2, 1, false, None);
        assert!(shared.ends_with("no handoff waiting"), "{shared}");
    }

    /// A marker written for a newer anamnesis still works, and the parts that
    /// did nothing have to be said out loud — a setting that is in the file
    /// and has no effect is invisible everywhere else.
    #[test]
    fn status_names_the_marker_tables_this_build_did_not_apply() {
        assert_eq!(describe_unrecognized(&[]), None);

        let one = describe_unrecognized(&["sessions".to_owned()]).expect("a line");
        assert!(one.contains("[sessions]"), "{one}");
        assert!(one.contains("is not understood"), "{one}");
        assert!(one.contains("upgrade anamnesis"), "{one}");

        let two = describe_unrecognized(&["sessions".to_owned(), "workstreams".to_owned()])
            .expect("a line");
        assert!(two.contains("[sessions], [workstreams]"), "{two}");
        assert!(two.contains("are not understood"), "{two}");
    }

    /// Silent when there is nothing waiting, because a line that is always
    /// there is a line nobody reads on the day it says something.
    #[test]
    fn capture_says_how_many_events_are_waiting() {
        let last = Some(at("2026-08-25T11:57:00Z"));
        let now = at("2026-08-25T12:00:00Z");

        assert!(!describe_capture(last, now, 0, 0).contains("waiting"));
        assert!(describe_capture(last, now, 1, 0).contains("1 event waiting"));
        assert!(describe_capture(last, now, 4, 0).contains("4 events waiting"));
    }

    /// The half of the queue that does not fix itself. Nothing will offer
    /// these again, so the line somebody reads while asking "is my work being
    /// recorded" is the only place they are ever mentioned.
    #[test]
    fn capture_says_how_many_events_were_refused() {
        let last = Some(at("2026-08-25T11:57:00Z"));
        let now = at("2026-08-25T12:00:00Z");

        assert!(!describe_capture(last, now, 0, 0).contains("refused"));
        assert!(describe_capture(last, now, 0, 1).contains("1 event the server refused"));
        assert!(describe_capture(last, now, 2, 3).contains("2 events waiting"));
        assert!(describe_capture(last, now, 2, 3).contains("3 events the server refused"));
    }
}
