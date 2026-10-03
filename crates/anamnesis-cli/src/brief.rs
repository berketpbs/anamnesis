//! A local, deterministic continuation file; no model call or handoff claim.
use crate::project::open_project;
use anamnesis_core::handoff::Slot;
use anamnesis_core::observation::EventKind;
use anamnesis_core::page::{PagePath, PageStatus};
use anamnesis_core::sanitize::Redactor;
use anamnesis_core::scope::OperatorName;
use anamnesis_wiki::Wiki;
use jiff::Timestamp;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Write a portable brief without changing the pending handoff.
pub fn cmd_brief(
    out: &Path,
    workstream: Option<&str>,
    operator: Option<&str>,
    force: bool,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, data, store) = open_project(data_dir)?;
    let stream = workstream
        .map(|name| {
            store
                .find_workstream(scope.project_id, name)?
                .map(|stream| stream.id)
                .ok_or_else(|| anyhow::anyhow!("no such workstream"))
        })
        .transpose()?;
    anyhow::ensure!(
        operator.is_none() || scope.slots.per_user,
        "--operator requires [slots] per_user = true"
    );
    // A per-user project must name a person; falling back to a shared session
    // would copy another person's checkpoint into a file that looks like theirs.
    anyhow::ensure!(
        !scope.slots.per_user || operator.is_some(),
        "name --operator for this per-user project"
    );
    let operator = operator.map(OperatorName::parse).transpose()?;
    let slot = Slot::for_workstream(stream).for_operator(operator.clone());
    let wiki = Wiki::open(data.wiki())?;
    let now = Timestamp::now();
    let mut text = format!(
        "# Continuation brief — {}\n\nGenerated {now}. Stored evidence to check; it may be out of date.\n",
        scope.scope
    );
    if let Some(name) = workstream {
        text.push_str(&format!("\nWorkstream: {name}\n"));
    }
    if let Some(name) = operator.as_ref() {
        text.push_str(&format!("\nOperator: {name}\n"));
    }
    text.push_str("\n## Last captured work\n");
    let current = store.latest_session_for_slot(scope.project_id, &slot)?;
    if let Some(id) = current {
        text.push_str(&format!(
            "\nSource session: `{id}` (`anamnesis show-session {id}`).\n"
        ));
        for (label, kind) in [
            ("Last request", EventKind::UserPrompt),
            ("Last assistant message", EventKind::AssistantMessage),
        ] {
            match store.latest_event_of_kind(scope.project_id, id, kind)? {
                Some(event) => {
                    text.push_str(&format!(
                        "\n### {label}\n\n{}\n\nSource event: `{}` at {}{}\n",
                        excerpt(event.body.as_str(), 4000),
                        event.id,
                        event.at,
                        if event.body.is_truncated() {
                            " (truncated at capture)"
                        } else {
                            ""
                        }
                    ));
                }
                None => text.push_str(&format!("\n{label}: not captured.\n")),
            }
        }
    } else {
        text.push_str("\nNo session captured in this slot.\n");
    }
    text.push_str("\n## Stored handoff: checkpoint and next step\n");
    match store
        .latest_handoff(scope.project_id, &slot)?
        .filter(|note| !note.dropped)
    {
        Some(note) => {
            text.push_str(&format!(
                "\n{}\n\nSource session: `{}`; note written {}.\n",
                excerpt(&note.body, 6000),
                note.from_session,
                note.written
            ));
            if Some(note.from_session) != current {
                text.push_str("\nThis note predates the latest captured session; check the newer source above.\n");
            }
        }
        None => text
            .push_str("\nNo retained handoff note in this slot. No next step has been inferred.\n"),
    }
    text.push_str("\n## Current decisions and recorded rejected approaches\n\nRejection reasons appear only where the stored page states them.\n");
    let mut included = 0;
    for decision in store.standing_decisions(scope.project_id, 200)? {
        let path = PagePath::parse(&decision.path)?;
        let page = wiki.read_versioned_page(&scope.scope, &path)?;
        let fm = &page.parsed.frontmatter;
        if fm.status != PageStatus::Active || fm.is_expired_at(now) {
            continue;
        }
        if let Some(source) = fm.session {
            match store.load_session(source)? {
                Some(source)
                    if source.project_id == scope.project_id
                        && source.workstream_id == stream
                        && operator
                            .as_ref()
                            .is_none_or(|name| source.operator.as_ref() == Some(name)) => {}
                _ => continue,
            }
        }
        let uri = reqwest::Url::from_file_path(wiki.locate(&scope.scope, &path))
            .map_err(|_| anyhow::anyhow!("cannot link source page"))?;
        text.push_str(&format!(
            "\n### {}\n\n{}\n\nSource: [{}]({uri}); revision `{}`.\n",
            fm.title,
            excerpt(&page.parsed.body, 3000),
            path,
            page.revision
        ));
        if let Some(id) = fm.session {
            text.push_str(&format!("Source session: `{id}`.\n"));
        }
        included += 1;
        if included == 20 {
            text.push_str("\nDecision listing is bounded to 20 pages.\n");
            break;
        }
    }
    if included == 0 {
        text.push_str("\nNo current decision pages found for this slot.\n");
    }
    let redactor = Redactor::new();
    let text = redactor.redact(&text).text().to_owned();
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(text.as_bytes())?;
    staged.as_file().sync_all()?;
    if force {
        staged.persist(out)?;
    } else {
        staged.persist_noclobber(out).map_err(|error| {
            anyhow::anyhow!(
                "output already exists or cannot be written; use --force to replace it: {}",
                error.error
            )
        })?;
    }
    println!(
        "Brief written to {}. The handoff slot is unchanged.",
        out.display()
    );
    Ok(())
}

fn excerpt(text: &str, budget: usize) -> String {
    let redacted = Redactor::new().redact(text).text().to_owned();
    let text = redacted.as_str();
    if text.len() <= budget {
        return text.to_owned();
    }
    let mut end = budget;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[excerpt truncated; open the source for more]",
        &text[..end]
    )
}
