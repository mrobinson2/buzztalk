//! Phrase segmentation: splitting a block of text into synthesis-sized
//! chunks so speech can start before the whole response has been generated.
//!
//! This is pure text processing — no model, no I/O — so it is exhaustively
//! unit tested here and runs in CI without the Pocket TTS bundle on disk.
//!
//! Rules, in priority order:
//!
//! 1. Terminal punctuation (`.` `!` `?`) always ends a chunk.
//! 2. Clause punctuation (`,` `;` `:`) ends a chunk only once the
//!    accumulated chunk is at least [`MIN_CLAUSE_SPLIT_LEN`] chars — below
//!    that, splitting hurts prosody more than it helps latency.
//! 3. A hard cap of roughly [`HARD_CAP_LEN`] chars forces a split at the
//!    next word boundary even with no punctuation in sight.
//! 4. The *first* chunk is special: it flushes at the first terminal
//!    punctuation **or** [`FIRST_CHUNK_MAX_LEN`] chars, whichever comes
//!    first, because time-to-first-audio dominates perceived latency.
//! 5. None of the above ever splits inside a fenced code block, a URL, the
//!    decimal point of a number, or a markdown table — those regions are
//!    marked "protected" up front and scanned over.

/// Minimum accumulated chunk length before clause punctuation is allowed to
/// end a chunk.
pub const MIN_CLAUSE_SPLIT_LEN: usize = 60;

/// Soft cap: once a chunk reaches this length, the next word boundary ends
/// it even without punctuation.
pub const HARD_CAP_LEN: usize = 200;

/// Absolute cap: if no word boundary shows up before this length (e.g. one
/// enormous unbroken token), split here anyway so a chunk can never grow
/// unboundedly. Kept well above [`HARD_CAP_LEN`] so it only fires on
/// pathological input.
const ABSOLUTE_CAP_LEN: usize = HARD_CAP_LEN + 40;

/// Length (or first terminal punctuation, if sooner) at which the very
/// first chunk is flushed. Time-to-first-audio dominates perceived latency,
/// so the first chunk is kept much shorter than later ones.
pub const FIRST_CHUNK_MAX_LEN: usize = 40;

/// Split `text` into ordered synthesis chunks.
///
/// Returns an empty vector for empty or all-whitespace input. Every
/// returned chunk is non-empty and trimmed of leading/trailing whitespace.
pub fn segment_into_phrases(text: &str) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let protected = protected_mask(&chars);
    split(&chars, &protected)
}

fn split(chars: &[char], protected: &[bool]) -> Vec<String> {
    let n = chars.len();
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    let mut first_chunk = true;

    while i < n {
        let c = chars[i];
        let in_protected = protected[i];
        let len_so_far = i - start + 1;

        if !in_protected && matches!(c, '.' | '!' | '?') {
            let end = i + 1;
            push_chunk(&mut chunks, chars, start, end);
            start = skip_ws(chars, end);
            i = start;
            first_chunk = false;
            continue;
        }

        if first_chunk && !in_protected && c.is_whitespace() && len_so_far > FIRST_CHUNK_MAX_LEN {
            let end = i;
            if end > start {
                push_chunk(&mut chunks, chars, start, end);
                start = skip_ws(chars, i);
                i = start;
                first_chunk = false;
                continue;
            }
        }

        if !first_chunk
            && !in_protected
            && matches!(c, ',' | ';' | ':')
            && len_so_far >= MIN_CLAUSE_SPLIT_LEN
        {
            let end = i + 1;
            push_chunk(&mut chunks, chars, start, end);
            start = skip_ws(chars, end);
            i = start;
            continue;
        }

        if !in_protected && c.is_whitespace() && len_so_far > HARD_CAP_LEN {
            let end = i;
            if end > start {
                push_chunk(&mut chunks, chars, start, end);
                start = skip_ws(chars, i);
                i = start;
                first_chunk = false;
                continue;
            }
        }

        if !in_protected && len_so_far >= ABSOLUTE_CAP_LEN {
            let end = i + 1;
            push_chunk(&mut chunks, chars, start, end);
            start = skip_ws(chars, end);
            i = start;
            first_chunk = false;
            continue;
        }

        i += 1;
    }

    if start < n {
        push_chunk(&mut chunks, chars, start, n);
    }
    chunks
}

fn push_chunk(chunks: &mut Vec<String>, chars: &[char], start: usize, end: usize) {
    let text: String = chars[start..end].iter().collect();
    let trimmed = text.trim();
    if !trimmed.is_empty() {
        chunks.push(trimmed.to_string());
    }
}

fn skip_ws(chars: &[char], mut i: usize) -> usize {
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Build a per-character "do not split here" mask covering fenced code
/// blocks, URLs, decimal numbers, and markdown table blocks.
fn protected_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    protect_code_fences(chars, &mut mask);
    protect_urls(chars, &mut mask);
    protect_numbers(chars, &mut mask);
    protect_tables(chars, &mut mask);
    mask
}

/// Mark every character inside (and including) a ```` ``` ```` … ```` ``` ````
/// fenced block as protected. An unterminated fence protects to the end of
/// the text rather than leaving it unbounded-scannable.
fn protect_code_fences(chars: &[char], mask: &mut [bool]) {
    let mut fence_starts = Vec::new();
    let mut i = 0;
    while i + 2 < chars.len() {
        if chars[i] == '`' && chars[i + 1] == '`' && chars[i + 2] == '`' {
            fence_starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut idx = 0;
    while idx + 1 < fence_starts.len() {
        let open = fence_starts[idx];
        let close_end = fence_starts[idx + 1] + 3;
        let bound = close_end.min(mask.len());
        for slot in mask.iter_mut().take(bound).skip(open) {
            *slot = true;
        }
        idx += 2;
    }
    if idx < fence_starts.len() {
        // Odd fence out: unterminated code block, protect to end of text.
        let open = fence_starts[idx];
        for slot in mask.iter_mut().skip(open) {
            *slot = true;
        }
    }
}

/// Mark URL-looking runs (`http://…`, `https://…`, `www.…`) as protected up
/// to the next whitespace.
fn protect_urls(chars: &[char], mask: &mut [bool]) {
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if mask[i] {
            i += 1;
            continue;
        }
        let starts_url = matches_at(chars, i, "http://")
            || matches_at(chars, i, "https://")
            || matches_at(chars, i, "www.");
        if starts_url {
            let mut end = i;
            while end < n && !chars[end].is_whitespace() {
                end += 1;
            }
            for slot in mask.iter_mut().take(end).skip(i) {
                *slot = true;
            }
            i = end;
        } else {
            i += 1;
        }
    }
}

fn matches_at(chars: &[char], pos: usize, needle: &str) -> bool {
    let needle: Vec<char> = needle.chars().collect();
    if pos + needle.len() > chars.len() {
        return false;
    }
    chars[pos..pos + needle.len()] == needle[..]
}

/// Mark decimal numbers (`123.45`, `1,234.5`) as protected so the decimal
/// point is never mistaken for terminal punctuation.
fn protect_numbers(chars: &[char], mask: &mut [bool]) {
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i].is_ascii_digit() {
            let run_start = i;
            let mut j = i;
            let mut saw_decimal_point = false;
            while j < n {
                let c = chars[j];
                if c.is_ascii_digit() {
                    j += 1;
                } else if (c == '.' || c == ',')
                    && j + 1 < n
                    && chars[j + 1].is_ascii_digit()
                    && j > run_start
                {
                    if c == '.' {
                        saw_decimal_point = true;
                    }
                    j += 1;
                } else {
                    break;
                }
            }
            if saw_decimal_point {
                for slot in mask.iter_mut().take(j).skip(run_start) {
                    *slot = true;
                }
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
}

/// Mark contiguous runs of markdown-table-looking lines (including the
/// blank space between them) as protected, so a split never lands inside a
/// table even at a row boundary.
fn protect_tables(chars: &[char], mask: &mut [bool]) {
    let mut line_start = 0usize;
    let mut lines = Vec::new(); // (start, end_exclusive_of_newline)
    for (idx, &c) in chars.iter().enumerate() {
        if c == '\n' {
            lines.push((line_start, idx));
            line_start = idx + 1;
        }
    }
    lines.push((line_start, chars.len()));

    let mut idx = 0;
    while idx < lines.len() {
        if is_table_line(chars, lines[idx]) {
            let block_start = lines[idx].0;
            let mut end_idx = idx;
            while end_idx < lines.len() && is_table_line(chars, lines[end_idx]) {
                end_idx += 1;
            }
            let block_end = lines[end_idx - 1].1;
            for slot in mask.iter_mut().take(block_end).skip(block_start) {
                *slot = true;
            }
            idx = end_idx;
        } else {
            idx += 1;
        }
    }
}

fn is_table_line(chars: &[char], (start, end): (usize, usize)) -> bool {
    let line: String = chars[start..end].iter().collect();
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let pipe_count = trimmed.chars().filter(|&c| c == '|').count();
    if pipe_count == 0 {
        return false;
    }
    // A separator row like `|---|:--:|` or `---|---`.
    let is_separator = trimmed.chars().all(|c| matches!(c, '-' | ':' | '|' | ' '));
    // A data/header row needs at least one pipe with non-space content on
    // both sides somewhere, i.e. it looks like `a | b`.
    is_separator || pipe_count >= 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_whitespace_only_produce_no_chunks() {
        assert_eq!(segment_into_phrases(""), Vec::<String>::new());
        assert_eq!(segment_into_phrases("   \n\t  "), Vec::<String>::new());
    }

    #[test]
    fn single_short_sentence_is_one_chunk() {
        assert_eq!(segment_into_phrases("Hello there."), vec!["Hello there."]);
    }

    #[test]
    fn no_terminal_punctuation_returns_whole_text_as_one_chunk_under_caps() {
        assert_eq!(
            segment_into_phrases("just some words"),
            vec!["just some words"]
        );
    }

    #[test]
    fn terminal_punctuation_always_splits_even_when_short() {
        let chunks = segment_into_phrases("Hi. Bye.");
        assert_eq!(chunks, vec!["Hi.", "Bye."]);
    }

    #[test]
    fn question_and_exclamation_marks_split_too() {
        let chunks = segment_into_phrases("Really? Yes! Okay.");
        assert_eq!(chunks, vec!["Really?", "Yes!", "Okay."]);
    }

    #[test]
    fn first_chunk_flushes_at_first_terminal_punctuation_before_forty_chars() {
        // "Sure." is well under 40 chars but has terminal punctuation, so it
        // becomes its own first chunk rather than merging with what follows.
        let chunks = segment_into_phrases(
            "Sure. Here is a longer second sentence that goes on for a while.",
        );
        assert_eq!(chunks[0], "Sure.");
    }

    #[test]
    fn first_chunk_flushes_at_forty_chars_when_no_early_terminal_punctuation() {
        // No terminal punctuation until well past 40 chars: the first
        // chunk should break at the next whitespace at/after 40 chars.
        let text = "this sentence deliberately has no punctuation for a long stretch of words before it finally ends";
        let chunks = segment_into_phrases(text);
        assert!(chunks[0].chars().count() > FIRST_CHUNK_MAX_LEN);
        assert!(
            chunks[0].chars().count() < 60,
            "first chunk grew unexpectedly large: {:?}",
            chunks[0]
        );
        assert!(text.starts_with(&chunks[0]));
    }

    #[test]
    fn clause_punctuation_does_not_split_below_min_len() {
        // "Well," is far short of MIN_CLAUSE_SPLIT_LEN, so the comma must
        // not end a chunk on its own; this all becomes the first chunk's
        // territory. First-chunk rule still applies at 40 chars/terminal.
        let chunks = segment_into_phrases("Well, I think so.");
        assert_eq!(chunks, vec!["Well, I think so."]);
    }

    #[test]
    fn clause_punctuation_splits_once_past_min_len_in_a_later_chunk() {
        let long_clause = "a".repeat(65);
        let text = format!(
            "Intro sentence ends here. {long_clause}, and then it keeps going until a final stop."
        );
        let chunks = segment_into_phrases(&text);
        // second logical chunk should have been cut at the comma
        assert!(chunks.iter().any(|c| c.ends_with(',')));
    }

    #[test]
    fn hard_cap_forces_a_split_with_no_punctuation() {
        let words = "lorem ".repeat(60); // ~360 chars, no punctuation at all
        let chunks = segment_into_phrases(&words);
        assert!(chunks.len() > 1, "expected multiple chunks, got {chunks:?}");
        for chunk in &chunks {
            assert!(
                chunk.chars().count() <= HARD_CAP_LEN + 10,
                "chunk exceeded hard cap: {} chars",
                chunk.chars().count()
            );
        }
    }

    #[test]
    fn absolute_cap_bounds_a_single_giant_token_with_no_whitespace() {
        let text = "a".repeat(500);
        let chunks = segment_into_phrases(&text);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.chars().count() <= HARD_CAP_LEN + 40);
        }
        assert_eq!(chunks.concat().chars().count(), 500);
    }

    #[test]
    fn never_splits_a_decimal_number() {
        let chunks = segment_into_phrases("The price is 3.14159 dollars exactly.");
        assert_eq!(chunks, vec!["The price is 3.14159 dollars exactly."]);
    }

    #[test]
    fn never_splits_inside_a_url() {
        let text = "Visit https://example.com/path.to.file for more info and details today.";
        let chunks = segment_into_phrases(text);
        let joined = chunks.join(" ");
        assert!(joined.contains("https://example.com/path.to.file"));
    }

    #[test]
    fn never_splits_inside_a_fenced_code_block() {
        let text = "Here is code: ```let x = 1.0; if x > 0.5 { print(x); }``` and then more text after it ends.";
        let chunks = segment_into_phrases(text);
        let has_intact_block = chunks
            .iter()
            .any(|c| c.contains("```let x = 1.0; if x > 0.5 { print(x); }```"));
        assert!(has_intact_block, "code block was split: {chunks:?}");
    }

    #[test]
    fn unterminated_code_fence_protects_to_end_of_text() {
        let text = "Before the fence. ```never closes. even with periods.";
        let chunks = segment_into_phrases(text);
        assert!(chunks
            .last()
            .unwrap()
            .contains("```never closes. even with periods."));
    }

    #[test]
    fn never_splits_inside_a_markdown_table() {
        let text = "Results:\n| A | B |\n|---|---|\n| 1.5 | 2.5 |\nDone now.";
        let chunks = segment_into_phrases(text);
        let has_intact_table = chunks
            .iter()
            .any(|c| c.contains("| A | B |") && c.contains("| 1.5 | 2.5 |"));
        assert!(has_intact_table, "table was split: {chunks:?}");
    }

    #[test]
    fn returned_chunks_are_trimmed_and_non_empty() {
        for chunk in segment_into_phrases("  Leading space. Trailing space too.  ") {
            assert_eq!(chunk, chunk.trim());
            assert!(!chunk.is_empty());
        }
    }

    #[test]
    fn multiple_sentences_each_become_their_own_chunk() {
        let text = "One. Two. Three. Four.";
        let chunks = segment_into_phrases(text);
        assert_eq!(chunks, vec!["One.", "Two.", "Three.", "Four."]);
    }

    #[test]
    fn realistic_llm_response_segments_sensibly() {
        let text = "Sure, I can help with that. The quick fix is to restart the service, \
                     which usually resolves transient connection errors, and then check the \
                     logs for anything unusual.";
        let chunks = segment_into_phrases(text);
        assert!(!chunks.is_empty());
        // First chunk should be short (time-to-first-audio).
        assert!(chunks[0].chars().count() <= 60);
        // Every chunk should be non-empty and end sensibly (not on raw
        // whitespace).
        for chunk in &chunks {
            assert!(!chunk.trim().is_empty());
        }
        // Reassembling with spaces should not have dropped any words.
        let word_count_in = text.split_whitespace().count();
        let word_count_out: usize = chunks.iter().map(|c| c.split_whitespace().count()).sum();
        assert_eq!(word_count_in, word_count_out);
    }
}
