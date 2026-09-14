//! Starting a server: the HTTP one hooks deliver to, and the MCP one an
//! agent talks to.
//!
//! One command, and most of it is about what the server is allowed to be. A
//! server with no token holds every prompt, every file path and every summary
//! of everyone who can reach the port; on loopback that boundary is the
//! machine, and off it, it is the network. So binding a non-loopback address
//! without a token is refused rather than warned about, and the startup line
//! says which of the two the server ended up being — the failure worth
//! preventing is a person who thinks their memory is private because nothing
//! said otherwise.

use std::path::PathBuf;
use std::sync::Arc;

use anamnesis_core::datadir::DataDir;
use anamnesis_core::scope::resolve_scope;
use anamnesis_store::Store;
use jiff::Timestamp;

use crate::format::describe_source;
use anamnesis_wiki::Wiki;

pub fn cmd_serve(
    bind: &str,
    port: u16,
    options: anamnesis_web::ServeOptions,
    allow_anonymous: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let address: std::net::SocketAddr = format!("{bind}:{port}").parse()?;

    // Read before anything is opened: a server whose tokens are misconfigured
    // should not have got as far as touching the data directory, and one that
    // would expose memory to a network should not start at all.
    let auth = anamnesis_web::Auth::from_env()?;
    if let Some(refusal) = refuse_anonymous_exposure(&address, auth.is_open(), allow_anonymous) {
        anyhow::bail!(refusal);
    }

    let data = DataDir::resolve(data_dir)?;

    // This line goes to the log file, which is the thing that outlives the
    // terminal: "when did memory stop" needs a first half to compare against.
    // It is written before anything that can fail rather than just before the
    // bind, so that a start that fails leaves a record of having been
    // attempted, which is the case where the file is the only place anybody
    // will look. It used to follow the model and the embedder; a server that
    // could not reach its embedder after a reboot then tried every minute and
    // left not one line.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        %address,
        data_dir = %data.root().display(),
        "anamnesis server starting"
    );

    data.ensure_layout()?;

    let store = Store::open(data.db_file())?;
    store.migrate()?;
    let wiki = Wiki::open(data.wiki())?;
    let raw = anamnesis_store::RawSpool::new(data.raw());

    // Built before the listener binds, so a misconfigured model is a startup
    // error someone sees rather than a warning that only surfaces hours later,
    // after sessions have already been summarised without one.
    // Unhurried on purpose: every model call this process makes is a session
    // summary, spawned and detached, with nothing holding a connection open
    // behind it. See `BACKGROUND_MAX_RETRIES`.
    let llm = llm_config(crate::settings::var)?;
    if let Some(key) = llm.ignored_key {
        tracing::warn!(
            "{key} is set and ANAMNESIS_LLM_PROVIDER does not name the provider it belongs to, \
             so it was not sent anywhere and pages are counted; name the provider in settings.env"
        );
    }
    // The same opt-in embedder the MCP server builds, on the same terms. Without
    // one here, the vector stream covered only the pages an agent wrote through
    // MCP — not a single session summary, and nothing anybody edited by hand.
    let embedder = embedder_for(
        &anamnesis_llm::EmbedConfig::from_vars(crate::settings::var),
        &data.models(),
        |said| tracing::warn!("{said}"),
    )?;
    let settings = llm.build()?.map(|provider| anamnesis_web::LlmSettings {
        provider,
        max_input_tokens: llm.max_input_tokens,
        max_output_tokens: llm.max_output_tokens,
    });

    let runtime = tokio::runtime::Runtime::new()?;

    // Bound before a word is printed about it. Everything below announces a
    // server that is serving, and until the listener exists that is a guess —
    // one this machine got wrong every minute for a while, printing the whole
    // banner and then failing on the address a healthy server already held.
    let listener = runtime
        .block_on(anamnesis_web::bind(address))
        .map_err(|error| explain_bind(address, &error))?;

    // The address in hand, rather than the one asked for: they differ when the
    // request was port 0, and every line below is read as a place to go.
    let address = listener.local_addr()?;

    println!("🌐 anamnesis serving on http://{address}");
    println!("   data dir: {}", data.root().display());
    println!("   POST /hook   GET /handoff   GET /whoami   GET /health");
    if options.ui {
        println!("   wiki browser: http://{address}/ui");
    }
    println!("   auth: {}", describe_serving_auth(&auth));
    println!(
        "   auto-improve: every {}s, for projects whose marker asks for it",
        anamnesis_web::improve::TICK.as_secs()
    );
    println!("   transcripts: {}", raw.root().display());
    println!("   logs:        {}", data.logs().display());
    println!(
        "   wiki edits:  {}",
        if options.watch_wiki {
            "watched — pages edited by hand are indexed as they are saved"
        } else {
            "not watched — hand edits need `anamnesis reindex`"
        }
    );
    match &settings {
        Some(settings) => println!("   consolidation: {}", settings.provider.describe()),
        None => println!("   consolidation: counted (no model configured)"),
    }
    match &embedder {
        Some(embedder) => println!("   embedding:     {}", embedder.model()),
        None => println!("   embedding:     off (set ANAMNESIS_EMBED_ENABLED=1)"),
    }
    // The same two facts into the log file. The banner goes to stdout, and a
    // server started by a service manager has nobody reading stdout: under
    // `conhost --headless` it goes nowhere at all. Which model and which
    // vectors a server started with is the first question when its pages
    // come out counted, and until now only a wrapper script that redirected
    // stdout could answer it.
    tracing::info!(
        consolidation = %settings
            .as_ref()
            .map_or_else(|| "counted (no model configured)".to_owned(), |s| s.provider.describe()),
        embedding = %embedder
            .as_ref()
            .map_or_else(|| "off".to_owned(), |e| e.model().to_owned()),
        "serving"
    );

    let served = runtime.block_on(anamnesis_web::serve_on(
        listener,
        anamnesis_web::AppState::new(store, wiki)
            .with_raw(Some(raw))
            .with_llm(settings)
            .with_auth(auth)
            .with_embedder(embedder),
        options,
    ));

    // The wiki watcher is a blocking task parked on a channel that never
    // closes, and dropping a runtime waits for blocking tasks to finish. Left
    // to drop, this one hangs the process *after* it has announced that it
    // stopped — the worst of both, a server that is not serving and not gone.
    //
    // It stayed hidden because the platform that reaches it first kills the
    // process anyway: Windows allows about five seconds after the console
    // closes and then terminates it, which is indistinguishable from exiting.
    // Ctrl-C has nothing to kill it, and hangs forever.
    //
    // Nothing is lost by not waiting for it. The index and the wiki are on
    // disk, and the work that was worth waiting for — sessions still being
    // summarised — `serve` waited for before it returned.
    runtime.shutdown_background();
    served?;
    Ok(())
}

/// The model settings this process runs on.
///
/// A named seam rather than a call inline in `cmd_serve`, which binds a
/// listener and so cannot be reached from a test. What is worth holding still
/// is the budget: every model call this process makes is a session summary,
/// spawned and detached, and a revert to the hurried default here would leave
/// all of `config.rs`'s tests passing.
fn llm_config(
    var: impl Fn(&str) -> Option<String>,
) -> Result<anamnesis_llm::LlmConfig, anamnesis_llm::LlmError> {
    anamnesis_llm::LlmConfig::from_vars_unhurried(var)
}

/// The embedder a process starts with, built before it serves anything.
///
/// Built up front, so a misconfigured embedder — a local model that will not
/// load, a hosted endpoint that refuses the key — is a startup error rather
/// than a warning buried in a log file.
///
/// Except a hosted endpoint that does not answer yet. That is not
/// misconfiguration; on the machine this project runs on it is an Ollama that
/// has not started, and after a reboot it is the ordinary state of things for
/// the first minute, or for good if nothing starts it. Refusing then costs far
/// more than the one retrieval stream allowed to be missing:
///
/// - the MCP server takes every memory tool away from the session it was
///   started for;
/// - the HTTP server takes capture itself. Under a service manager the refusal
///   went nowhere anybody reads — `conhost --headless` discards stderr and
///   reports the exit as success — and the one-minute restart asked again,
///   failed again, and recorded nothing for as long as Ollama stayed down.
///
/// So both start with an embedder that connects when it is first needed. The
/// reason is said through `said`, and a page written before the endpoint
/// answers records its embedding failure, which `doctor` reports with
/// `anamnesis reindex` as the remedy.
///
/// The commands a person runs for one page or one query take it on the same
/// terms — `search`, `write-page`, `bootstrap` — since for each of them the
/// vector is one signal among several. `write-page` had it worst: it committed
/// the page to the wiki and then failed building the embedder, so the page was
/// in the wiki and missing from the index. `reindex` and `reconsolidate` keep
/// refusing, because filling in vectors is much of what a rebuild is run for,
/// and a rebuild that quietly recorded a failure for every page would be worse
/// than one that stopped.
pub(crate) fn embedder_for(
    config: &anamnesis_llm::EmbedConfig,
    models: &std::path::Path,
    said: impl FnOnce(&str),
) -> anyhow::Result<Option<Arc<dyn anamnesis_llm::Embedder>>> {
    match config.build(models) {
        Ok(embedder) => Ok(embedder),
        Err(error) if config.provider == anamnesis_llm::embed::EmbedProvider::Hosted => {
            said(&format!(
                "{} did not answer ({error}); going on without vectors, \
                 asking again when one is needed",
                config.url
            ));
            Ok(Some(Arc::new(anamnesis_llm::hosted::Reconnecting::new(
                config.url.clone(),
                config.model.clone(),
                config.key.clone(),
            ))))
        }
        Err(error) => Err(error.into()),
    }
}

/// What to say when the address will not be taken.
///
/// The operating system's own sentence is the entire message today, and it is
/// written in the language the machine was installed in. On the machine this
/// was found on it reads `Normal olarak her yuva adresi ... icin yalnizca bir
/// kullanima izin veriliyor. (os error 10048)` — which names neither anamnesis
/// nor the address, says nothing about what to do, and cannot be searched for
/// by anybody whose machine speaks differently.
///
/// So each of these says what happened in terms of the thing every one of them
/// is about: the address. The operating system's text is kept, at the end,
/// because it is still the ground truth and an error number is what somebody
/// will paste into a search.
fn explain_bind(address: std::net::SocketAddr, error: &std::io::Error) -> anyhow::Error {
    use std::io::ErrorKind;

    match error.kind() {
        ErrorKind::AddrInUse => anyhow::anyhow!(
            "{address} is already taken — something is listening there. If that is anamnesis, this one was not needed: `anamnesis status` says whether a server is up. If it is not, `--port` takes another. ({error})"
        ),
        ErrorKind::AddrNotAvailable => anyhow::anyhow!(
            "{address} is not an address this machine answers on. `--bind` wants one that is; the default, 127.0.0.1, always is. ({error})"
        ),
        ErrorKind::PermissionDenied => anyhow::anyhow!(
            "not allowed to listen on {address}. Ports below 1024 belong to privileged processes on most systems, so `--port` above them is the usual answer. ({error})"
        ),
        _ => anyhow::anyhow!("could not listen on {address}: {error}"),
    }
}

/// Refuse to serve a network address with nothing guarding it.
///
/// The default bind is loopback, where the machine's own boundary is the whole
/// story and a token would only be ceremony — which is why an open server stays
/// legal there, and why every install that predates tokens keeps working. An
/// address reachable from elsewhere is a different proposition: what is behind
/// this port is every prompt someone typed, every path they opened, and every
/// summary written from them. Refusing is recoverable in one command; the
/// alternative failure is silent and permanent.
///
/// `--allow-anonymous` exists because "in front of a proxy that authenticates"
/// is a real deployment, and a check with no way past it gets worked around by
/// worse means.
fn refuse_anonymous_exposure(
    address: &std::net::SocketAddr,
    open: bool,
    allow_anonymous: bool,
) -> Option<String> {
    if !open || allow_anonymous || address.ip().is_loopback() {
        return None;
    }

    let token_env = anamnesis_web::auth::TOKEN_ENV;
    Some(format!(
        "refusing to serve {address} with no token configured.\n\n\
         Everything this server holds — every prompt, every file path, every\n\
         summary written from them — would be readable by anything that can\n\
         reach that address.\n\n\
         Mint one with `anamnesis token`, then set {token_env} for this server\n\
         and for whatever runs the hooks. Or pass --allow-anonymous to serve\n\
         it open anyway."
    ))
}

/// The startup line for what the server accepts.
fn describe_serving_auth(auth: &anamnesis_web::Auth) -> String {
    if auth.is_open() {
        return "open — no token required".to_owned();
    }

    let named: Vec<String> = auth.named().map(ToString::to_string).collect();
    match named.len() {
        0 => "token required".to_owned(),
        _ => format!("token required ({})", named.join(", ")),
    }
}

/// Start the MCP server bound to `repo`'s scope, speaking stdio.
///
/// One process per project: the scope is resolved once, at startup, the same
/// way `serve` binds one store and wiki rather than re-resolving per request.
/// A harness that wants a different project starts a different process.
pub fn cmd_mcp(repo: &std::path::Path, data_dir: Option<PathBuf>) -> anyhow::Result<()> {
    let repo = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let scope = resolve_scope(&repo)?;
    let data = DataDir::resolve(data_dir)?;
    data.ensure_layout()?;

    let store = Store::open(data.db_file())?;
    store.migrate()?;
    store.upsert_project(&scope, Timestamp::now())?;
    let wiki = Wiki::open(data.wiki())?;

    // Never stdout, for the reason below.
    let embedder = embedder_for(
        &anamnesis_llm::EmbedConfig::from_vars(crate::settings::var),
        &data.models(),
        |said| eprintln!("anamnesis: {said}"),
    )?;

    // Never stdout: the MCP transport owns stdout for protocol frames, so a
    // stray print here would corrupt the stream the same way a log line would
    // corrupt the `hook` command's handoff channel.
    eprintln!(
        "anamnesis: mcp server for {} ({})",
        scope.scope,
        describe_source(&scope.source)
    );
    eprintln!(
        "   vector search: {}",
        match &embedder {
            Some(embedder) => format!("enabled ({})", embedder.model()),
            None => "disabled (set ANAMNESIS_EMBED_ENABLED=1 to turn on)".to_owned(),
        }
    );

    let server = anamnesis_mcp::AnamnesisMcp::new(store, wiki, scope, repo).with_embedder(embedder);

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        use rmcp::ServiceExt;
        let service = server.serve(rmcp::transport::stdio()).await?;
        service.waiting().await?;
        anyhow::Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(raw: &str) -> std::net::SocketAddr {
        raw.parse().expect("address")
    }

    /// Loopback is where every single-user install lives, and where the port
    /// is the boundary. Refusing there would break them all for no gain.
    #[test]
    fn an_open_server_on_loopback_is_allowed_to_start() {
        assert!(refuse_anonymous_exposure(&address("127.0.0.1:8080"), true, false).is_none());
        assert!(refuse_anonymous_exposure(&address("[::1]:8080"), true, false).is_none());
    }

    #[test]
    fn an_open_server_on_a_network_address_is_refused_and_told_how() {
        let refusal = refuse_anonymous_exposure(&address("0.0.0.0:8080"), true, false)
            .expect("should refuse");
        assert!(refusal.contains("anamnesis token"), "{refusal}");
        assert!(refusal.contains("--allow-anonymous"), "{refusal}");
    }

    /// The refusal is about being unguarded, not about the address: a server
    /// with tokens configured may bind wherever it likes.
    #[test]
    fn a_guarded_server_may_serve_any_address() {
        assert!(refuse_anonymous_exposure(&address("0.0.0.0:8080"), false, false).is_none());
    }

    /// "Behind a proxy that authenticates" is a real deployment, and a check
    /// with no way past it gets worked around by worse means.
    #[test]
    fn the_refusal_can_be_overridden_deliberately() {
        assert!(refuse_anonymous_exposure(&address("0.0.0.0:8080"), true, true).is_none());
    }

    fn hosted_at(url: String) -> anamnesis_llm::EmbedConfig {
        anamnesis_llm::EmbedConfig {
            enabled: true,
            provider: anamnesis_llm::embed::EmbedProvider::Hosted,
            model: "nomic-embed-text".to_owned(),
            url,
            key: None,
        }
    }

    /// The morning this was found: the machine rebooted, Ollama had not
    /// started, and the server refused to start every minute without a line
    /// anywhere. An endpoint that does not answer is a port nothing listens
    /// on — held for a moment and let go, so the refusal is real and immediate.
    #[test]
    fn an_embedder_that_does_not_answer_yet_does_not_stop_a_start() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|listener| listener.local_addr())
            .expect("a free port")
            .port();
        let url = format!("http://127.0.0.1:{port}/v1/embeddings");
        let models = tempfile::tempdir().expect("models dir");

        let mut said = None;
        let embedder = embedder_for(&hosted_at(url.clone()), models.path(), |line| {
            said = Some(line.to_owned());
        })
        .expect("a start that does not depend on the endpoint");

        let embedder = embedder.expect("an embedder that connects when it is needed");
        assert_eq!(embedder.model(), "nomic-embed-text");
        let said = said.expect("the reason is said, not swallowed");
        assert!(said.contains(&url), "{said}");
        assert!(said.contains("without vectors"), "{said}");
    }

    /// Off is off: nothing is built and nothing is said.
    #[test]
    fn an_embedder_nobody_turned_on_is_neither_built_nor_mentioned() {
        let models = tempfile::tempdir().expect("models dir");
        let config = anamnesis_llm::EmbedConfig {
            enabled: false,
            ..hosted_at("http://127.0.0.1:9/v1/embeddings".to_owned())
        };

        let mut said = false;
        let embedder = embedder_for(&config, models.path(), |_| said = true).expect("off");

        assert!(embedder.is_none());
        assert!(!said);
    }

    /// The reason this change exists, asserted where the choice is made: the
    /// server waits longer than a caller holding a connection open would.
    /// `config.rs` proves the two budgets differ; only this proves `serve`
    /// takes the one nobody is waiting on.
    #[test]
    fn the_server_does_not_hurry_a_summary_nobody_waits_for() {
        let waiting = anamnesis_llm::LlmConfig::from_vars(|_| None).expect("defaults");
        let ours = llm_config(|_| None).expect("defaults");

        assert!(
            ours.max_retries > waiting.max_retries,
            "serve retried {} times, no better than a caller who is waiting",
            ours.max_retries
        );
    }

    /// And an operator who has chosen a number still keeps it here.
    #[test]
    fn an_explicit_retry_setting_still_wins_in_the_server() {
        let config = llm_config(|key| (key == "ANAMNESIS_LLM_MAX_RETRIES").then(|| "1".to_owned()))
            .expect("explicit retries");

        assert_eq!(config.max_retries, 1);
    }

    #[test]
    fn the_startup_line_names_the_operators_it_will_accept() {
        let auth = anamnesis_web::Auth::parse(None, Some("alice=alpha,bob=beta")).expect("parse");
        let line = describe_serving_auth(&auth);
        assert!(line.contains("alice"), "{line}");
        assert!(line.contains("bob"), "{line}");

        let shared = anamnesis_web::Auth::parse(Some("alpha"), None).expect("parse");
        assert_eq!(describe_serving_auth(&shared), "token required");
        assert!(describe_serving_auth(&anamnesis_web::Auth::open()).contains("open"));
    }

    /// The failure this change is for, against the real operating system error
    /// rather than one made up to match the branch. A listener is held, the
    /// same address is asked for again, and what a person would read is
    /// checked for the two things the bare OS text has never had: which
    /// address, and what to do next.
    #[test]
    fn an_address_already_held_is_refused_with_something_to_act_on() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let held = runtime
            .block_on(anamnesis_web::bind(address("127.0.0.1:0")))
            .expect("first bind");
        let taken = held.local_addr().expect("local address");

        let error = runtime
            .block_on(anamnesis_web::bind(taken))
            .expect_err("the address is held");
        let message = explain_bind(taken, &error).to_string();

        assert!(message.contains(&taken.to_string()), "{message}");
        assert!(message.contains("anamnesis status"), "{message}");
    }

    /// Port 0 means "whichever is free", and the banner is a list of places to
    /// go. Printing the request rather than the result would send somebody to
    /// port 0, which is not an address at all.
    #[test]
    fn the_address_in_hand_is_the_one_worth_printing() {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let listener = runtime
            .block_on(anamnesis_web::bind(address("127.0.0.1:0")))
            .expect("bind");

        let taken = listener.local_addr().expect("local address");
        assert_ne!(taken.port(), 0, "the banner would have said http://{taken}");
    }

    /// The other two failures a bind has, which cannot be provoked from a test
    /// on every machine: a bind address belonging to somewhere else, and a
    /// port this process is not allowed to have. Each names the flag that
    /// changes it, because that is the whole difference from the OS text.
    #[test]
    fn the_other_refusals_name_the_flag_that_answers_them() {
        use std::io::{Error, ErrorKind};

        let elsewhere = explain_bind(
            address("10.1.2.3:8080"),
            &Error::from(ErrorKind::AddrNotAvailable),
        )
        .to_string();
        assert!(elsewhere.contains("10.1.2.3:8080"), "{elsewhere}");
        assert!(elsewhere.contains("--bind"), "{elsewhere}");

        let privileged = explain_bind(
            address("127.0.0.1:80"),
            &Error::from(ErrorKind::PermissionDenied),
        )
        .to_string();
        assert!(privileged.contains("127.0.0.1:80"), "{privileged}");
        assert!(privileged.contains("--port"), "{privileged}");
    }

    /// Anything else still names the address, which is the part the operating
    /// system leaves out of every one of these.
    #[test]
    fn an_unrecognised_failure_still_says_where() {
        let message = explain_bind(
            address("127.0.0.1:8080"),
            &std::io::Error::from(std::io::ErrorKind::Other),
        )
        .to_string();

        assert!(message.contains("127.0.0.1:8080"), "{message}");
    }
}
