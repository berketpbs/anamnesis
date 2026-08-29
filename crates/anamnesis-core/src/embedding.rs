//! What indexing needs from an embedder.
//!
//! Narrower than [`anamnesis_llm::Embedder`] on purpose, and in a different
//! crate for a reason: the model lives behind candle, and the index has no
//! business depending on a machine-learning toolchain to store a vector
//! somebody else computed. This is the seam — a name to record alongside the
//! vector, and a function that produces one.
//!
//! The name matters more than it looks. Two embedding models produce vectors
//! in unrelated spaces, and cosine similarity between them is a number with no
//! meaning at all rather than an error. Recording which model wrote a vector is
//! what lets a query compare only against its own kind.
//!
//! [`anamnesis_llm::Embedder`]: https://docs.rs/anamnesis-llm

/// Something that turns a page into a vector, for whoever is doing the writing.
pub trait Embed: Send + Sync {
    /// Model identifier, stored beside every vector this produces.
    fn model(&self) -> &str;

    /// Embed one piece of text, or say why not.
    ///
    /// The error is a string because nothing above this cares which layer of a
    /// model failed: an embedding that does not happen costs a page its place
    /// in one retrieval stream, and is logged rather than propagated.
    fn embed(&self, text: &str) -> Result<Vec<f32>, String>;
}

/// The text a page is embedded as.
///
/// One definition, because a page embedded from its title and a query embedded
/// from a question have to be comparable, and because two call sites that
/// disagreed about whether the title is included would fill one index with two
/// kinds of vector and no way to tell them apart.
pub fn page_text(title: &str, body: &str) -> String {
    format!("{title}\n\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_is_embedded_as_its_title_and_body() {
        assert_eq!(
            page_text("Why SQLite", "One file."),
            "Why SQLite\n\nOne file."
        );
    }
}
