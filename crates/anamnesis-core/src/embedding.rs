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

/// How much of a text did not fit the model that embedded it.
///
/// A model with a fixed context does not fail on a longer input; it embeds the
/// part it can reach. The resulting vector is a perfectly ordinary vector, and
/// nothing downstream can tell it apart from one that represents the whole
/// page — which is why this type exists rather than the overflow being left
/// implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overflow {
    /// Tokens the text came to.
    pub tokens: usize,
    /// Tokens the model can attend to.
    pub budget: usize,
}

impl Overflow {
    /// Tokens that did not reach the model.
    pub fn dropped(&self) -> usize {
        self.tokens.saturating_sub(self.budget)
    }

    /// Share of the text the vector actually represents, in `[0, 1]`.
    pub fn covered(&self) -> f64 {
        if self.tokens == 0 {
            return 1.0;
        }
        (self.budget.min(self.tokens) as f64) / (self.tokens as f64)
    }
}

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

    /// Whether this text is longer than the model can read, and by how much.
    ///
    /// Asked *by the writer*, rather than counted there, for the reason this
    /// whole module exists: the index has no business depending on a tokenizer
    /// to find out what a tokenizer already knows, and an estimate made from
    /// character counts would be an estimate of a different model's appetite.
    /// Only the embedder can answer exactly, so only the embedder is asked.
    ///
    /// `None` means "it fits", or "this embedder cannot say" — deliberately
    /// the same answer, because a caller that could tell them apart would have
    /// to decide which one to report, and there is nothing useful to report
    /// about an embedder that does not know its own limit. Defaulted so that
    /// an implementation which has never thought about it keeps compiling and
    /// keeps quiet, rather than claiming a page fits when nobody checked.
    fn overflow(&self, _text: &str) -> Option<Overflow> {
        None
    }
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
    fn an_overflow_says_how_much_did_not_reach_the_model() {
        let over = Overflow {
            tokens: 1341,
            budget: 512,
        };
        assert_eq!(over.dropped(), 829);
        assert!((over.covered() - 512.0 / 1341.0).abs() < 1e-12);
    }

    /// `dropped` is saturating and `covered` is capped, so a caller that
    /// builds one of these for a text that fits gets "nothing missing" rather
    /// than a negative count or a coverage above one. Nothing should build
    /// such a value — `overflow` returns `None` instead — but a report that
    /// printed `-12 tokens did not reach the model` would be worse than the
    /// silence this work replaced.
    #[test]
    fn a_text_that_fits_is_reported_as_whole() {
        let fits = Overflow {
            tokens: 100,
            budget: 512,
        };
        assert_eq!(fits.dropped(), 0);
        assert_eq!(fits.covered(), 1.0);
    }

    /// Empty text divides by zero on the way to a coverage, and the honest
    /// answer for "how much of nothing is represented" is all of it.
    #[test]
    fn an_empty_text_is_covered_rather_than_undefined() {
        let empty = Overflow {
            tokens: 0,
            budget: 512,
        };
        assert_eq!(empty.covered(), 1.0);
        assert_eq!(empty.dropped(), 0);
    }

    /// The default is silence. An embedder that has never thought about its
    /// own window must not be read as promising that everything fits.
    #[test]
    fn an_embedder_that_does_not_implement_overflow_says_nothing() {
        struct Silent;
        impl Embed for Silent {
            fn model(&self) -> &str {
                "silent-1"
            }
            fn embed(&self, _text: &str) -> Result<Vec<f32>, String> {
                Ok(vec![0.0])
            }
        }
        assert_eq!(Silent.overflow(&"x".repeat(100_000)), None);
    }

    #[test]
    fn a_page_is_embedded_as_its_title_and_body() {
        assert_eq!(
            page_text("Why SQLite", "One file."),
            "Why SQLite\n\nOne file."
        );
    }
}
