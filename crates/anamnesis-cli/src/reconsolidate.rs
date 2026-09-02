//! Summarise finished sessions again, this time with a model.
//!
//! A session is summarised once, at the moment it ends, by whatever the server
//! had available then. A server with no model still writes a page — that is
//! deliberate, and it is why capture never depends on a provider — but what it
//! writes is a count: which prompts arrived, which files were named, how many
//! times each tool ran. The reasoning, the decisions and the dead ends are in
//! the observations and go no further.
//!
//! That page is not wrong, and nothing later replaces it. Turning a model on
//! improves every session from that day forward and leaves the ones already
//! written exactly as they are, which is the wrong way round: the sessions
//! worth reading are usually the old ones. The observations are still in the
//! index, so the summary can simply be asked for again.
//!
//! What this does not touch is as deliberate as what it does. See
//! `pipeline::recompile` for why a recompiled session leaves no handoff and
//! keeps the time it actually ended.

use std::path::PathBuf;

use anamnesis_consolidate::consolidate_with_llm;
use anamnesis_core::audit::Action;
use anamnesis_core::embedding::Embed;
use anamnesis_core::page::{PagePath, Tier};
use anamnesis_core::scope::ResolvedScope;
use anamnesis_store::{SessionSummary, Store};
use anamnesis_wiki::Wiki;
use jiff::Timestamp;

use crate::audit::note;
use crate::project::open_project;
use crate::sessions::one_session;

/// A session that will be summarised again, and the page it will overwrite.
struct Candidate {
    session: SessionSummary,
    path: PagePath,
    title: String,
}

/// A session that will be left alone, and why.
struct Skipped {
    session: SessionSummary,
    reason: String,
}

/// Ask a model to summarise finished sessions again.
pub fn cmd_reconsolidate(
    prefixes: &[String],
    apply: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;

    // Built first, and refused first. Recompiling without a model would
    // replace a counted page with a counted page: the wiki would gain a
    // commit, every page would look freshly written to the sweep, and not one
    // word would have changed. This command exists to add what a model says,
    // so having no model is the one condition under which it does nothing.
    let config = anamnesis_llm::LlmConfig::from_env()?;
    let Some(provider) = config.build()? else {
        println!("♻  Recompiling {}", scope.scope);
        println!();
        println!("  No model is configured, so there is nothing to add.");
        println!("  Set ANTHROPIC_API_KEY (or ANAMNESIS_LLM_PROVIDER with its own key)");
        println!("  in this shell and run this again. `anamnesis status` reports what");
        println!("  the server compiles with, which is configured separately.");
        return Ok(());
    };

    let wiki = Wiki::open(data.wiki())?;
    let (candidates, skipped) = select(&store, &wiki, &scope, prefixes)?;

    println!("♻  Recompiling {}", scope.scope);
    println!("   model {}", provider.name());
    println!();

    if candidates.is_empty() && skipped.is_empty() {
        println!("  No finished session in this project has a page to rewrite.");
        return Ok(());
    }

    if candidates.is_empty() {
        println!("  Nothing to recompile.");
    } else {
        let verb = if apply {
            "Recompiling"
        } else {
            "Would recompile"
        };
        println!("  {verb} {} session(s):", candidates.len());
        let width = candidates
            .iter()
            .map(|item| item.path.as_str().len())
            .max()
            .unwrap_or(0)
            .min(60);
        for item in &candidates {
            println!(
                "    {:width$}  {:>4} obs  {}",
                item.path.as_str(),
                item.session.observation_count,
                item.title
            );
        }
    }

    if !skipped.is_empty() {
        println!();
        println!("  Leaving {} alone:", skipped.len());
        for item in &skipped {
            let short: String = item.session.id.to_string().chars().take(8).collect();
            println!("    {short}  {}", item.reason);
        }
    }

    if !apply {
        if !candidates.is_empty() {
            println!();
            println!("  Nothing has been written. Re-run with --apply to carry this out.");
        }
        return Ok(());
    }

    if candidates.is_empty() {
        return Ok(());
    }

    // The same opt-in embedder the server and `reindex` build. Without it a
    // rewritten page keeps the vector its old body was embedded into, and the
    // one stream that finds pages by meaning would be answering from prose
    // that is no longer on the page.
    let embedder = anamnesis_llm::EmbedConfig::from_env().build(&data.models())?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let preferences = anamnesis_web::read_preferences(&wiki, &scope);
    let now = Timestamp::now();
    let mut written = 0usize;
    let mut refused = 0usize;

    println!();
    for item in &candidates {
        let Some(session) = store.load_session(item.session.id)? else {
            continue;
        };
        let observations = store.observations(item.session.id)?;

        let digest = runtime.block_on(consolidate_with_llm(
            provider.as_ref(),
            &session,
            &observations,
            preferences.as_deref(),
            config.max_input_tokens,
            config.max_output_tokens,
        ));

        // A model that times out, refuses, or answers with something that is
        // not a digest is not an error here, for the reason it is not one
        // during capture: the page that already exists is still true. One
        // session keeps the summary it had and the rest carry on.
        let Some(digest) = digest else {
            println!("  ✗ {}  the model gave nothing back", item.path.as_str());
            refused += 1;
            continue;
        };

        let path = anamnesis_web::recompile(
            &store,
            &wiki,
            &scope,
            &session,
            &digest,
            embedder.as_ref().map(|inner| inner.as_ref() as &dyn Embed),
            now,
        )?;

        note(
            &store,
            Some(scope.project_id),
            Action::SessionRecompiled,
            path.clone(),
            Some(format!("{} observation(s)", observations.len())),
        );

        println!("  ✓ {path}  {}", digest.title);
        written += 1;
    }

    let left = match refused {
        0 => String::new(),
        n => format!(", {n} left as they were"),
    };
    println!();
    println!("  {written} page(s) rewritten{left}");
    println!();
    println!("  Every page they replaced is still in the wiki's git history:");
    println!("  git -C {} log -p sessions/", wiki.root().display());

    Ok(())
}

/// Decide which sessions get asked about again, and which are left alone.
///
/// Ownership is the dividing line, and it is read from the page rather than
/// from the index: a page somebody pinned, marked canonical, or moved out of
/// the episodic tier is one a person has taken a decision about, and a rewrite
/// would quietly undo it. `improve` reads pages from the wiki for the same
/// reason — the index is a derived copy, and a copy is not where an intention
/// is expressed.
fn select(
    store: &Store,
    wiki: &Wiki,
    scope: &ResolvedScope,
    prefixes: &[String],
) -> anyhow::Result<(Vec<Candidate>, Vec<Skipped>)> {
    let named = !prefixes.is_empty();
    let sessions = if named {
        // Every prefix is resolved before anything is written, for the reason
        // `forget-session` resolves them all first: stopping at the third name
        // after acting on two leaves somebody to work out which was the typo.
        let mut chosen: Vec<SessionSummary> = Vec::with_capacity(prefixes.len());
        for prefix in prefixes {
            let session = one_session(store, scope, prefix)?;
            if chosen.iter().any(|seen| seen.id == session.id) {
                continue;
            }
            chosen.push(session);
        }
        chosen
    } else {
        store.recent_sessions(scope.project_id, usize::MAX)?
    };

    let mut candidates = Vec::new();
    let mut skipped = Vec::new();

    for session in sessions {
        let path = anamnesis_web::session_page_path(&session.started_at, session.id)?;

        if !wiki.exists(&scope.scope, &path) {
            // An open session has not finished saying what it is about, and a
            // closed one with no page held nothing but its own boundaries.
            // Neither earns a line unless somebody asked for that session by
            // name and is owed an answer about why nothing happened to it.
            if named {
                skipped.push(Skipped {
                    session,
                    reason: "no page — the session left nothing to rewrite".to_owned(),
                });
            }
            continue;
        }

        let parsed = match wiki.read_page(&scope.scope, &path) {
            Ok(parsed) => parsed,
            Err(error) => {
                skipped.push(Skipped {
                    session,
                    reason: format!("its page could not be read: {error}"),
                });
                continue;
            }
        };

        let owned = if parsed.frontmatter.pinned {
            Some("pinned")
        } else if parsed.frontmatter.canonical {
            Some("canonical")
        } else if parsed.frontmatter.tier != Tier::Episodic {
            Some("promoted out of the episodic tier")
        } else {
            None
        };

        match owned {
            Some(reason) => skipped.push(Skipped {
                session,
                reason: format!("{} is {reason}", path.as_str()),
            }),
            None => candidates.push(Candidate {
                title: parsed.frontmatter.title.clone(),
                session,
                path,
            }),
        }
    }

    Ok((candidates, skipped))
}
