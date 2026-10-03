//! Project-scoped accounting and pruning of inactive embedding models.
use crate::{Result, Store};
use anamnesis_core::ids::ProjectId;
use rusqlite::params;

/// Stored index material under one embedding model.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct VectorModel {
    /// Embedding model identifier.
    pub model: String,
    /// Whole-page and section vectors.
    pub page_vectors: usize,
    /// Abstract vectors.
    pub abstract_vectors: usize,
    /// Retained failure rows.
    pub failures: usize,
    /// Stored vector blob bytes, excluding database overhead.
    pub bytes: usize,
}

impl Store {
    /// Count vectors and failure remnants per model in this project.
    pub fn vector_models(&self, project: ProjectId) -> Result<Vec<VectorModel>> {
        let conn = self.connection();
        let mut stmt = conn.prepare(
            "SELECT model, sum(v), sum(a), sum(f), sum(bytes) FROM (
               SELECT e.model, 1 v, 0 a, 0 f, length(e.vector) bytes FROM page_embeddings e JOIN pages p ON p.id = e.page_id WHERE p.project_id = ?1
               UNION ALL
               SELECT e.model, 0, 1, 0, length(e.vector) FROM page_abstract_embeddings e JOIN pages p ON p.id = e.page_id WHERE p.project_id = ?1
               UNION ALL
               SELECT e.model, 0, 0, 1, 0 FROM page_embed_failures e JOIN pages p ON p.id = e.page_id WHERE p.project_id = ?1
             ) GROUP BY model ORDER BY model",
        )?;
        let rows = stmt.query_map([project.to_string()], |row| {
            Ok(VectorModel {
                model: row.get(0)?,
                page_vectors: row.get(1)?,
                abstract_vectors: row.get(2)?,
                failures: row.get(3)?,
                bytes: row.get(4)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Remove one model's index rows atomically, protecting every named active model.
    ///
    /// A protected model returns zero and is never deleted. Wiki, raw storage,
    /// other projects and downloaded model files are outside this operation.
    pub fn prune_vector_model(
        &self,
        project: ProjectId,
        model: &str,
        protected: &[String],
    ) -> Result<usize> {
        if protected.iter().any(|name| name == model) {
            return Ok(0);
        }
        let mut conn = self.connection();
        let tx = conn.transaction()?;
        let mut removed = 0;
        for table in [
            "page_embeddings",
            "page_abstract_embeddings",
            "page_embed_failures",
        ] {
            removed += tx.execute(&format!("DELETE FROM {table} WHERE model = ?1 AND page_id IN (SELECT id FROM pages WHERE project_id = ?2)"),
                params![model, project.to_string()])?;
        }
        tx.commit()?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anamnesis_core::page::{Frontmatter, Page, PagePath};
    use anamnesis_core::scope::resolve_scope;
    use jiff::Timestamp;

    #[test]
    fn pruning_is_project_scoped_and_protects_all_active_index_material() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in_memory().unwrap();
        store.migrate().unwrap();
        let now = Timestamp::now();
        let mut projects = Vec::new();
        for name in ["first", "second"] {
            let repo = dir.path().join(name);
            std::fs::create_dir(&repo).unwrap();
            std::fs::write(
                repo.join(".anamnesis.toml"),
                format!("[scope]\nworkspace = \"test\"\nproject = \"{name}\"\n"),
            )
            .unwrap();
            let scope = resolve_scope(&repo).unwrap();
            store.upsert_project(&scope, now).unwrap();
            let page = Page::new(
                scope.project_id,
                PagePath::parse("decisions/storage.md").unwrap(),
                Frontmatter::new("storage", vec![]).unwrap(),
                "preserved wiki content",
            );
            store.upsert_page(&page, now).unwrap();
            for model in ["active", "old"] {
                store.set_page_embedding(page.id, model, &[1., 0.]).unwrap();
                store
                    .set_page_sections(page.id, model, &[vec![1., 0.], vec![0., 1.]])
                    .unwrap();
                store
                    .set_abstract_embedding(page.id, model, &[1., 0.])
                    .unwrap();
                store
                    .record_embed_failure(page.id, model, "retained failure")
                    .unwrap();
            }
            projects.push(scope.project_id);
        }
        let before = store.vector_models(projects[0]).unwrap();
        assert_eq!(before.len(), 2);
        assert_eq!(before[0].page_vectors, 3);
        assert_eq!(before[0].abstract_vectors, 1);
        assert_eq!(before[0].failures, 1);
        assert_eq!(before[0].bytes, 32);
        assert_eq!(
            store
                .prune_vector_model(projects[0], "active", &["active".into()])
                .unwrap(),
            0
        );
        assert_eq!(store.vector_models(projects[0]).unwrap(), before);
        assert_eq!(
            store
                .prune_vector_model(projects[0], "old", &["active".into()])
                .unwrap(),
            5
        );
        assert_eq!(
            store.vector_models(projects[0]).unwrap(),
            vec![before[0].clone()]
        );
        assert_eq!(store.vector_models(projects[1]).unwrap(), before);
        assert_eq!(store.page_count(projects[0]).unwrap(), 1);
        assert_eq!(
            store
                .prune_vector_model(projects[0], "old", &["active".into()])
                .unwrap(),
            0
        );
    }
}
