//! `anamnesis improve`: what a pass proposes, and who decides.
//!
//! The pass never acts on its own unless a project has said it may. Left
//! alone it files proposals and stops, and every change is made by a person
//! naming one — because the changes on offer are the ones that put a page out
//! of the decay sweep's reach, and a system that quietly promotes its own
//! pages is one nobody can reason about later.

use std::path::PathBuf;

use anamnesis_store::Store;
use anamnesis_wiki::Wiki;
use jiff::Timestamp;

use crate::format::describe_age;
use crate::project::open_project;

/// Run an improvement pass, and show what is waiting on a person.
///
/// The same pass the server runs on a schedule, on demand. Which is the point
/// of it being a command as well: `[auto_improve.scheduler]` is off by
/// default, so for most projects this is the only thing that ever looks.
/// Run a learning pass, and carry out or dismiss what it proposed.
pub fn cmd_improve(
    apply: Option<String>,
    dismiss: Option<String>,
    history: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let now = Timestamp::now();

    if let Some(prefix) = apply {
        let wiki = Wiki::open(data.wiki())?;
        return decide_by_applying(&store, &wiki, &scope, &prefix, now);
    }
    if let Some(prefix) = dismiss {
        return decide_by_dismissing(&store, &scope, &prefix, now);
    }

    println!("🌱 Improving {}", scope.scope);
    println!();

    let wiki = Wiki::open(data.wiki())?;
    let report = anamnesis_web::improve::run_pass(
        &store,
        &wiki,
        &scope.scope,
        scope.project_id,
        &scope.auto_improve,
        now,
    )?;

    let Some(report) = report else {
        println!("  Auto-improve is off for this project.");
        println!(
            "  Set `[auto_improve] enabled = true` in {} to turn it on.",
            marker_name(&scope)
        );
        return Ok(());
    };
    store.mark_improved(scope.project_id, now)?;

    println!(
        "  {} noticed, {} refreshed, {} resolved.",
        report.filed.filed, report.filed.refreshed, report.filed.resolved
    );

    for carried in &report.carried {
        let done = match &carried.outcome {
            anamnesis_web::improve::Outcome::Promoted { commit } => {
                format!("promoted (commit {})", &commit[..commit.len().min(8)])
            }
            anamnesis_web::improve::Outcome::AlreadyDurable => {
                "already promoted by someone else".to_owned()
            }
            anamnesis_web::improve::Outcome::NeedsAPerson => "needs a person".to_owned(),
        };
        println!("  ✓ {} — {done}", carried.subject);
    }
    for (subject, error) in &report.failures {
        println!("  ⚠ {subject} — {error}");
    }

    print_proposals(&store, &scope, history, now)?;

    if report.open > 0 && scope.auto_improve.require_approval {
        println!();
        println!("  This project requires approval, so nothing was changed.");
        println!("  anamnesis improve --apply <id>    carry one out");
        println!("  anamnesis improve --dismiss <id>  never propose it again");
    }

    Ok(())
}

/// Print the proposals a project is sitting on.
fn print_proposals(
    store: &Store,
    scope: &anamnesis_core::scope::ResolvedScope,
    history: bool,
    now: Timestamp,
) -> anyhow::Result<()> {
    let proposals = store.proposals(scope.project_id, !history)?;
    if proposals.is_empty() {
        println!();
        println!("  Nothing to propose. The memory is in good shape.");
        return Ok(());
    }

    let mut open_shown = false;
    let mut decided_shown = false;
    for proposal in &proposals {
        if proposal.state.is_open() && !open_shown {
            println!();
            println!("  Open:");
            open_shown = true;
        }
        if !proposal.state.is_open() && !decided_shown {
            println!();
            println!("  Decided:");
            decided_shown = true;
        }

        let short = proposal.id.to_string();
        let age = describe_age(proposal.created_at, now);
        println!(
            "    {}  {:<28}  {}",
            &short[..8],
            proposal.kind.action(),
            proposal.subject
        );
        println!("              {}", proposal.rationale);
        if proposal.state.is_open() {
            println!("              noticed {age}");
        } else {
            println!("              {} {age}", proposal.state.as_str());
        }
    }
    Ok(())
}

/// Carry out one proposal, named by any unambiguous prefix of its id.
fn decide_by_applying(
    store: &Store,
    wiki: &Wiki,
    scope: &anamnesis_core::scope::ResolvedScope,
    prefix: &str,
    now: Timestamp,
) -> anyhow::Result<()> {
    let proposal = one_proposal(store, scope, prefix)?;
    anyhow::ensure!(
        proposal.state.is_open(),
        "that proposal was already {} — a decision is made once",
        proposal.state.as_str()
    );

    match anamnesis_web::improve::apply(
        store,
        wiki,
        &scope.scope,
        scope.project_id,
        &proposal,
        now,
    )? {
        anamnesis_web::improve::Outcome::Promoted { commit } => {
            println!("✓ Promoted {} to the semantic tier", proposal.subject);
            println!("  commit {}", &commit[..commit.len().min(8)]);
            println!();
            println!("  It is now exempt from the decay sweep.");
        }
        anamnesis_web::improve::Outcome::AlreadyDurable => {
            println!("· {} was already promoted", proposal.subject);
            println!("  The proposal is resolved; nothing was written.");
        }
        anamnesis_web::improve::Outcome::NeedsAPerson => {
            println!("· Nothing here can be done mechanically.");
            println!("  {}: {}", proposal.kind.action(), proposal.subject);
            println!("  {}", proposal.rationale);
            println!();
            println!("  Left open. Write it with `anamnesis write-page`, or dismiss it.");
        }
    }
    Ok(())
}

/// Dismiss one proposal, so no later pass files it again.
fn decide_by_dismissing(
    store: &Store,
    scope: &anamnesis_core::scope::ResolvedScope,
    prefix: &str,
    now: Timestamp,
) -> anyhow::Result<()> {
    let proposal = one_proposal(store, scope, prefix)?;
    anyhow::ensure!(
        store.decide_proposal(
            proposal.id,
            anamnesis_core::improve::ProposalState::Dismissed,
            now
        )?,
        "that proposal was already {} — a decision is made once",
        proposal.state.as_str()
    );

    println!("· Dismissed: {}", proposal.subject);
    println!("  Later passes will notice the same thing and leave it alone.");
    Ok(())
}

/// Resolve an id prefix to exactly one proposal.
///
/// Refuses an ambiguous prefix rather than acting on whichever row sorted
/// first: these decisions are permanent, and "it picked the other one" is not
/// a mistake anyone can undo.
fn one_proposal(
    store: &Store,
    scope: &anamnesis_core::scope::ResolvedScope,
    prefix: &str,
) -> anyhow::Result<anamnesis_store::StoredProposal> {
    let mut matches = store.proposals_matching(scope.project_id, prefix)?;
    match matches.len() {
        0 => anyhow::bail!("no proposal in {} starts with {prefix:?}", scope.scope),
        1 => Ok(matches.remove(0)),
        _ => {
            let listed: Vec<String> = matches
                .iter()
                .map(|proposal| format!("{} ({})", &proposal.id.to_string()[..8], proposal.subject))
                .collect();
            anyhow::bail!(
                "{prefix:?} matches {}: {}",
                matches.len(),
                listed.join(", ")
            )
        }
    }
}

/// The marker file to point someone at, by the name it actually has.
fn marker_name(scope: &anamnesis_core::scope::ResolvedScope) -> String {
    match &scope.marker {
        Some(path) => path.display().to_string(),
        None => ".anamnesis.toml".to_owned(),
    }
}
