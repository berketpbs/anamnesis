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

/// The most sections one page is embedded in.
///
/// A bound on what a single write can cost. Every section is a pass through the
/// model, and nothing caps how long a page written through MCP can be; a page
/// that would need more than this keeps its one whole-page vector and the
/// truncation row that says what that vector stands for, which is a thin page
/// reported as thin rather than a write that took a minute. At the size this
/// wiki's pages actually are — the longest measured came to about 1,300 tokens,
/// eleven sections at most — the bound does not bind.
pub const MAX_SECTIONS: usize = 64;

/// The texts a page is embedded as when the whole of it does not fit.
///
/// A model with a fixed window reads the start of a long page and nothing
/// after it, so one vector per page stands for its opening. This cuts the page
/// into pieces the model reads *whole*, each carrying the page's title and the
/// heading it sits under, so that a piece from the middle of a page still says
/// which page and which part of it it is:
///
/// 1. The body is split at markdown headings. A `#` inside a fenced code block
///    is not a heading, and a heading with nothing under it is carried onto the
///    next one rather than dropped.
/// 2. Within a section, paragraphs are packed into as few pieces as fit.
/// 3. A paragraph that does not fit on its own is split by lines, then
///    sentences, then words, then characters, and packed the same way.
///
/// `fits` is the embedder's own answer — [`Embed::overflow`] returning `None` —
/// because only the tokenizer knows what fits. It is asked by binary search
/// over how many units a piece takes, so a long section costs a handful of
/// tokenizations rather than one per word.
///
/// Every word of the body appears in some piece, in order: nothing is dropped
/// to make the pieces fit, which is the whole of the difference from the
/// single vector this sits beside.
pub fn page_sections(title: &str, body: &str, fits: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut carried: Option<String> = None;

    let sections = split_sections(body);
    let last = sections.len().saturating_sub(1);
    for (index, section) in sections.into_iter().enumerate() {
        let heading = match (carried.take(), section.heading) {
            (Some(above), Some(here)) => Some(format!("{above}\n{here}")),
            (above, here) => here.or(above),
        };
        if section.blocks.is_empty() {
            // A heading with nothing under it belongs with whatever comes next;
            // at the very end there is nothing next, so it is packed as content
            // — split like any other text if a run of them outgrew the window.
            match heading {
                Some(heading) if index == last => {
                    let (prefix, _) = prefix_for(title, None, fits);
                    pack(&prefix, vec![heading], Grain::Block, fits, &mut pieces);
                }
                heading => carried = heading,
            }
            continue;
        }

        // A heading that cannot ride in every piece's prefix is not dropped:
        // it becomes the section's first block, and is packed and split like
        // one.
        let (prefix, heading_in_prefix) = prefix_for(title, heading.as_deref(), fits);
        let mut blocks = section.blocks;
        if let (Some(heading), false) = (heading, heading_in_prefix) {
            blocks.insert(0, heading);
        }
        pack(&prefix, blocks, Grain::Block, fits, &mut pieces);
    }
    pieces
}

/// The part of a page between one heading and the next.
struct Section {
    heading: Option<String>,
    blocks: Vec<String>,
}

/// Split a markdown body into sections of blocks.
fn split_sections(body: &str) -> Vec<Section> {
    let mut sections = vec![Section {
        heading: None,
        blocks: Vec::new(),
    }];
    let mut block = String::new();
    let mut fence: Option<&'static str> = None;

    let flush = |block: &mut String, sections: &mut Vec<Section>| {
        let text = block.trim();
        if !text.is_empty() {
            sections
                .last_mut()
                .expect("there is always a section")
                .blocks
                .push(text.to_owned());
        }
        block.clear();
    };

    for line in body.lines() {
        let trimmed = line.trim_start();

        if let Some(marker) = fence {
            block.push_str(line);
            block.push('\n');
            if trimmed.starts_with(marker) {
                fence = None;
                flush(&mut block, &mut sections);
            }
            continue;
        }

        let indent = line.len() - trimmed.len();
        if indent < 4 && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
            flush(&mut block, &mut sections);
            fence = Some(if trimmed.starts_with("```") {
                "```"
            } else {
                "~~~"
            });
            block.push_str(line);
            block.push('\n');
            continue;
        }

        if indent < 4 && is_heading(trimmed) {
            flush(&mut block, &mut sections);
            sections.push(Section {
                heading: Some(trimmed.trim_end().to_owned()),
                blocks: Vec::new(),
            });
            continue;
        }

        if trimmed.is_empty() {
            flush(&mut block, &mut sections);
            continue;
        }

        block.push_str(line);
        block.push('\n');
    }
    flush(&mut block, &mut sections);
    sections
}

/// An ATX heading: one to six `#`, then a space or the end of the line.
fn is_heading(trimmed: &str) -> bool {
    let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
    (1..=6).contains(&hashes) && (trimmed.len() == hashes || trimmed[hashes..].starts_with(' '))
}

/// What every piece of a section begins with — the page title and the heading,
/// or as much of that as leaves room for content — and whether the heading is
/// part of it.
///
/// A title or heading too long to fit beside anything would otherwise make
/// every character of the body its own piece. When the heading is what has to
/// go, the caller packs it as content instead, so its words are still read.
fn prefix_for(title: &str, heading: Option<&str>, fits: &dyn Fn(&str) -> bool) -> (String, bool) {
    let mut candidates = Vec::new();
    if let Some(heading) = heading {
        candidates.push((format!("{title}\n\n{heading}\n\n"), true));
        candidates.push((format!("{heading}\n\n"), true));
    }
    candidates.push((format!("{title}\n\n"), false));

    candidates
        .into_iter()
        .find(|(prefix, _)| fits(&format!("{prefix}xxxxxxxx")))
        .unwrap_or_default()
}

/// How finely a piece of text is being split, coarsest first.
#[derive(Debug, Clone, Copy)]
enum Grain {
    Block,
    Line,
    Sentence,
    Word,
    Character,
}

impl Grain {
    fn separator(self) -> &'static str {
        match self {
            Self::Block => "\n\n",
            Self::Line => "\n",
            Self::Sentence | Self::Word => " ",
            Self::Character => "",
        }
    }

    fn finer(self) -> Option<Self> {
        match self {
            Self::Block => Some(Self::Line),
            Self::Line => Some(Self::Sentence),
            Self::Sentence => Some(Self::Word),
            Self::Word => Some(Self::Character),
            Self::Character => None,
        }
    }

    /// Split one unit of the coarser grain into units of this one.
    fn split(self, text: &str) -> Vec<String> {
        match self {
            Self::Block => vec![text.to_owned()],
            Self::Line => text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
            Self::Sentence => split_sentences(text),
            Self::Word => text.split_whitespace().map(str::to_owned).collect(),
            Self::Character => text.chars().map(String::from).collect(),
        }
    }
}

/// Sentences, cut after `.`, `!` or `?` followed by whitespace.
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        if matches!(character, '.' | '!' | '?')
            && chars.peek().is_some_and(|(_, next)| next.is_whitespace())
        {
            let end = index + character.len_utf8();
            let sentence = text[start..end].trim();
            if !sentence.is_empty() {
                sentences.push(sentence.to_owned());
            }
            start = end;
        }
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        sentences.push(rest.to_owned());
    }
    sentences
}

/// Pack units into as few pieces as fit, splitting any unit that cannot.
fn pack(
    prefix: &str,
    units: Vec<String>,
    grain: Grain,
    fits: &dyn Fn(&str) -> bool,
    pieces: &mut Vec<String>,
) {
    let separator = grain.separator();
    let piece = |from: usize, count: usize| {
        format!("{prefix}{}", units[from..from + count].join(separator))
    };

    let mut start = 0;
    while start < units.len() {
        if !fits(&piece(start, 1)) {
            match grain.finer() {
                Some(finer) => pack(prefix, finer.split(&units[start]), finer, fits, pieces),
                // One character that does not fit beside the prefix. The prefix
                // was chosen to leave room, so this is a tokenizer disagreeing
                // with itself; the model reads what it can rather than the
                // page losing the character.
                None => pieces.push(piece(start, 1)),
            }
            start += 1;
            continue;
        }

        // The largest count that still fits: `low` always fits, `high` is the
        // most there are.
        let (mut low, mut high) = (1, units.len() - start);
        while low < high {
            let middle = (low + high).div_ceil(2);
            if fits(&piece(start, middle)) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        pieces.push(piece(start, low));
        start += low;
    }
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

    /// A window counted in whitespace words, which is enough to reason about
    /// here: nothing in this module depends on what a token is.
    fn words_under(limit: usize) -> impl Fn(&str) -> bool {
        move |text: &str| text.split_whitespace().count() <= limit
    }

    #[test]
    fn a_page_is_cut_at_its_headings_and_every_piece_says_where_it_is() {
        let body = "Opening words here.\n\n## Cause\n\nThe counter reset.\n\n## Fix\n\nPersist it.";
        let pieces = page_sections("Postmortem", body, &words_under(50));

        assert_eq!(
            pieces,
            [
                "Postmortem\n\nOpening words here.",
                "Postmortem\n\n## Cause\n\nThe counter reset.",
                "Postmortem\n\n## Fix\n\nPersist it.",
            ]
        );
    }

    #[test]
    fn paragraphs_share_a_piece_while_they_fit() {
        let body = "one two three\n\nfour five six\n\nseven eight nine";
        // Title (1) plus two paragraphs (6) is seven words; all three is ten.
        let pieces = page_sections("T", body, &words_under(7));
        assert_eq!(
            pieces,
            [
                "T\n\none two three\n\nfour five six",
                "T\n\nseven eight nine"
            ]
        );
    }

    #[test]
    fn a_paragraph_too_long_for_the_window_is_split_until_it_fits() {
        let body = "First sentence has five words. Second one is also quite long here. Third.";
        let limit = 6;
        let pieces = page_sections("T", body, &words_under(limit));

        assert!(pieces.len() > 1, "{pieces:?}");
        for piece in &pieces {
            assert!(piece.split_whitespace().count() <= limit, "{piece:?}");
            assert!(piece.starts_with("T\n\n"), "{piece:?}");
        }
    }

    /// A single word longer than the window is still read — in characters —
    /// rather than cut off where the model stops.
    #[test]
    fn a_word_longer_than_the_window_is_split_into_characters() {
        let fits = |text: &str| text.chars().count() <= 12;
        let pieces = page_sections("T", "abcdefghijklmnopqrstuvwxyz", &fits);

        assert!(pieces.iter().all(|piece| fits(piece)), "{pieces:?}");
        let joined: String = pieces
            .iter()
            .map(|piece| piece.trim_start_matches("T\n\n"))
            .collect();
        assert_eq!(joined, "abcdefghijklmnopqrstuvwxyz");
    }

    #[test]
    fn a_heading_inside_code_is_not_a_heading() {
        let body = "Setup:\n\n```sh\n# install it\ncargo install x\n```\n\nDone.";
        let pieces = page_sections("T", body, &words_under(100));
        assert_eq!(pieces.len(), 1, "{pieces:?}");
        assert!(pieces[0].contains("# install it"), "{pieces:?}");
    }

    /// A heading directly above another one is not lost; it rides along with
    /// the next section's pieces. At the end of a page there is nothing to
    /// ride with, so it is a piece of its own.
    #[test]
    fn a_heading_with_nothing_under_it_is_kept() {
        let body = "## Part one\n### Detail\n\nThe text.\n\n## Trailing";
        let pieces = page_sections("T", body, &words_under(100));
        assert_eq!(
            pieces,
            [
                "T\n\n## Part one\n### Detail\n\nThe text.",
                "T\n\n## Trailing",
            ]
        );
    }

    /// A title that fills the window on its own would make every character a
    /// piece. It is dropped from the prefix instead.
    #[test]
    fn a_title_too_long_to_fit_beside_anything_is_left_out_of_the_pieces() {
        let title = "word ".repeat(20);
        let pieces = page_sections(title.trim(), "short body text", &words_under(10));
        assert_eq!(pieces, ["short body text"]);
    }

    #[test]
    fn an_empty_body_has_no_pieces() {
        assert!(page_sections("T", "", &words_under(10)).is_empty());
        assert!(page_sections("T", "\n\n   \n", &words_under(10)).is_empty());
    }
}
