//! Keeping a prompt inside a context window without a tokenizer.
//!
//! Shipping a real tokenizer would mean shipping a vocabulary per provider and
//! keeping them current — a large dependency in service of a bound that only
//! needs to be *safe*, not exact. So this estimates, and estimates high.
//!
//! Three characters per token is deliberately pessimistic. English prose runs
//! closer to four, but the material here is the opposite of English prose:
//! file paths, JSON fragments, tool names, and — in this project's own
//! sessions — Turkish, which tokenizes worse than English on every model
//! anamnesis talks to. Over-estimating costs a slightly shorter prompt.
//! Under-estimating costs a 400 at the moment a session ends, which is the
//! one moment there is no second chance.

/// Characters assumed to make one token.
const CHARS_PER_TOKEN: usize = 3;

/// Roughly how many tokens a string will cost.
///
/// Counts characters rather than bytes: a multi-byte character is still one
/// character to a tokenizer, and counting bytes would over-charge non-ASCII
/// text by a factor of two or three.
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(CHARS_PER_TOKEN)
}

/// Cut `text` down to roughly `max_tokens`, at a character boundary.
///
/// Truncation is marked. A summary built from silently truncated input is
/// worse than one built from visibly truncated input, because only the second
/// kind tells the model — and later, the reader — that something is missing.
pub fn clip_to_tokens(text: &str, max_tokens: usize) -> String {
    if estimate_tokens(text) <= max_tokens {
        return text.to_owned();
    }

    const MARKER: &str = "\n[… truncated to fit the model's context …]";
    let marker_tokens = estimate_tokens(MARKER);
    let room = max_tokens.saturating_sub(marker_tokens) * CHARS_PER_TOKEN;

    let mut out: String = text.chars().take(room).collect();
    out.push_str(MARKER);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_string_costs_nothing() {
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn estimation_rounds_up_so_short_strings_are_never_free() {
        assert_eq!(estimate_tokens("ab"), 1);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
    }

    #[test]
    fn multibyte_text_is_counted_by_character_not_byte() {
        // Six characters, twelve bytes. Charging by byte would double the cost
        // of every Turkish session summary.
        let text = "çşğüöı";
        assert_eq!(text.len(), 12);
        assert_eq!(estimate_tokens(text), 2);
    }

    #[test]
    fn text_within_budget_is_returned_untouched() {
        let text = "short enough";
        assert_eq!(clip_to_tokens(text, 100), text);
    }

    #[test]
    fn clipping_discloses_itself() {
        let text = "x".repeat(3_000);
        let clipped = clip_to_tokens(&text, 100);
        assert!(clipped.contains("truncated"));
        assert!(estimate_tokens(&clipped) <= 100);
    }

    #[test]
    fn clipping_never_splits_a_character() {
        let text = "ı".repeat(3_000);
        let clipped = clip_to_tokens(&text, 50);
        // The point is that this does not panic and the result is valid UTF-8
        // with whole characters — slicing by byte index would do neither.
        assert!(clipped.starts_with('ı'));
        assert!(estimate_tokens(&clipped) <= 50);
    }

    #[test]
    fn a_budget_smaller_than_the_marker_still_terminates() {
        let clipped = clip_to_tokens(&"x".repeat(100), 1);
        assert!(clipped.contains("truncated"));
    }
}
