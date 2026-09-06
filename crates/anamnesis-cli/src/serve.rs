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
    let llm = anamnesis_llm::LlmConfig::from_env_unhurried()?;
    // The same opt-in embedder the MCP server builds. Without one here, the
    // vector stream covered only the pages an agent wrote through MCP — not a
    // single session summary, and nothing anybody edited by hand.
    let embedder = anamnesis_llm::EmbedConfig::from_env().build(&data.models())?;
    let settings = llm.build()?.map(|provider| anamnesis_web::LlmSettings {
        provider,
        max_input_tokens: llm.max_input_tokens,
        max_output_tokens: llm.max_output_tokens,
    });

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
        Some(settings) => println!(
            "   consolidation: {} ({})",
            settings.provider.model(),
            settings.provider.name()
        ),
        None => println!("   consolidation: counted (no model configured)"),
    }
    match &embedder {
        Some(embedder) => println!("   embedding:     {}", embedder.model()),
        None => println!("   embedding:     off (set ANAMNESIS_EMBED_ENABLED=1)"),
    }

    // The banner above goes to the terminal this was started from, which is
    // exactly the thing that will not exist later. This line goes to the file,
    // so that "when did memory stop" has a first half to compare against.
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        %address,
        data_dir = %data.root().display(),
        "anamnesis server starting"
    );

    let runtime = tokio::runtime::Runtime::new()?;
    let served = runtime.block_on(anamnesis_web::serve(
        address,
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

    // Built before the transport connects, so a misconfigured or unreachable
    // model is a startup error someone sees rather than a warning buried in a
    // log file, the same reasoning `cmd_serve` applies to the LLM provider.
    let embed_config = anamnesis_llm::EmbedConfig::from_env();
    let embedder = embed_config.build(&data.models())?;

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
}
