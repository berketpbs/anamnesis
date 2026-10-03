//! Preview obsolete embedding rows, and prune only explicitly when requested.
use crate::project::open_project;
use anamnesis_llm::EmbedConfig;
use std::path::PathBuf;

/// Preview or remove inactive model rows while preserving configured and live models.
pub fn cmd_prune(
    model: Option<&str>,
    apply: bool,
    server: &str,
    data_dir: Option<PathBuf>,
) -> anyhow::Result<()> {
    let (scope, _data, store) = open_project(data_dir)?;
    let config = EmbedConfig::from_vars(crate::settings::var);
    let mut protected = vec![config.model.clone()];
    if let Some(short) = config.model.rsplit('/').next() {
        protected.push(short.to_owned());
    }
    let live = crate::doctor::server_whoami(server);
    let live_model = live.as_ref().and_then(|body| body.get("embedding"));
    if let Some(name) = live_model.and_then(serde_json::Value::as_str) {
        protected.push(name.to_owned());
    }
    protected.sort();
    protected.dedup();
    if let Some(model) = model {
        anyhow::ensure!(
            !protected.iter().any(|name| name == model),
            "requested model is protected by configured or live embedding selection"
        );
    }
    println!("Protected embedding models: {}", protected.join(", "));
    if live_model.is_none() {
        println!(
            "Live embedding selection is unknown; apply is refused until the server reports it."
        );
    }
    let candidates: Vec<_> = store
        .vector_models(scope.project_id)?
        .into_iter()
        .filter(|item| {
            !protected.contains(&item.model) && model.is_none_or(|name| name == item.model)
        })
        .collect();
    if candidates.is_empty() {
        println!("No inactive model rows selected.");
        return Ok(());
    }
    for item in &candidates {
        println!(
            "{}: {} page/section vectors, {} abstract vectors, {} failures, {} vector bytes",
            item.model, item.page_vectors, item.abstract_vectors, item.failures, item.bytes
        );
    }
    if !apply {
        println!("Preview only. Run again with --apply to remove these project-scoped index rows.");
        return Ok(());
    }
    anyhow::ensure!(
        live_model.is_some_and(
            |value| value.is_null() || value.as_str().is_some_and(|name| !name.is_empty())
        ),
        "cannot confirm live embedding selection; start the matching server or choose --server before applying"
    );
    // A server replaced between preview and mutation cannot silently change
    // which model the command treats as active.
    let again = crate::doctor::server_whoami(server);
    anyhow::ensure!(
        again.as_ref().and_then(|body| body.get("embedding")) == live_model,
        "live embedding selection changed; inspect a new preview before applying"
    );
    let mut removed = 0;
    for item in candidates {
        removed += store.prune_vector_model(scope.project_id, &item.model, &protected)?;
    }
    println!("Removed {removed} inactive index rows. Wiki and raw sources are unchanged.");
    Ok(())
}
