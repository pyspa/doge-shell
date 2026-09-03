//! Splits a stream of Markdown text into confirmed top-level blocks.
//!
//! [`super::render_markdown`] is a whole-document, batch renderer: its
//! `TerminalRenderer` is private and `finish(self)` consumes it, so there is
//! no way to feed it partial text and get partial output. Rather than make
//! that renderer incremental, this module solves a narrower problem: decide
//! which *prefix* of the text received so far is safe to hand to
//! [`super::render_markdown`] right now, because nothing that arrives later
//! could change how it parses.
//!
//! The boundary this looks for is a blank line between two top-level
//! blocks - except when the block before the blank line is a list, table,
//! or blockquote, since a loose list's items are themselves separated by
//! blank lines and confirming too early would cut it apart mid-list. A
//! fenced code block is also held back in full, since a partial fence
//! parses as nothing like the real thing.
//!
//! What this deliberately does *not* try to reproduce is the blank-line
//! *spacing* between rendered blocks: `TerminalRenderer::ensure_blank_line`
//! inserts exactly one blank line between any two top-level blocks
//! regardless of their kind (the only exception is before the very first
//! block, which gets none), so the caller can always join
//! `render_markdown(confirmed_block)` outputs with a literal blank line and
//! match a whole-document render byte for byte. See the `stream_matches_*`
//! tests in this module, which check exactly that.

/// Above this many pending characters, a block is flushed even without a
/// clean boundary, so one very long paragraph cannot hold up the whole
/// display. This can introduce one extra blank line that a whole-document
/// render would not have (the paragraph split in two looks like two
/// paragraphs) - an accepted tradeoff for bounded memory and latency.
const MAX_PENDING_BLOCK_CHARS: usize = 4000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LineKind {
    Heading,
    ListItem,
    TableRow,
    BlockquoteLine,
    /// Indented by 4+ columns: a continuation line inside a list item, or an
    /// indented code block when nothing precedes it.
    IndentedContinuation,
    Text,
}

/// Whether a blank line between a `prev`-kind line and a `next`-kind line
/// separates two blocks, or merely two pieces of one loose list, table, or
/// blockquote.
fn coalesces_across_blank(prev: LineKind, next: LineKind) -> bool {
    use LineKind::*;
    matches!(
        (prev, next),
        (ListItem, ListItem)
            | (ListItem, IndentedContinuation)
            | (IndentedContinuation, ListItem)
            | (IndentedContinuation, IndentedContinuation)
            | (TableRow, TableRow)
            | (BlockquoteLine, BlockquoteLine)
    )
}

fn classify(line: &str) -> LineKind {
    let trimmed = line.trim_start();
    let indent = line.chars().count() - trimmed.chars().count();

    if indent >= 4 {
        return LineKind::IndentedContinuation;
    }
    if let Some(rest) = trimmed.strip_prefix('#') {
        let hashes = 1 + rest.chars().take_while(|&c| c == '#').count();
        let after = &trimmed[hashes.min(trimmed.len())..];
        if (1..=6).contains(&hashes) && (after.is_empty() || after.starts_with(char::is_whitespace))
        {
            return LineKind::Heading;
        }
    }
    if trimmed.starts_with('>') {
        return LineKind::BlockquoteLine;
    }
    if is_list_marker(trimmed) {
        return LineKind::ListItem;
    }
    if trimmed.contains('|') {
        return LineKind::TableRow;
    }
    LineKind::Text
}

fn is_list_marker(trimmed: &str) -> bool {
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(c @ ('-' | '*' | '+')) => {
            let rest = &trimmed[c.len_utf8()..];
            rest.is_empty() || rest.starts_with(char::is_whitespace)
        }
        Some(c) if c.is_ascii_digit() => {
            let digits_end = trimmed
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(trimmed.len());
            let rest = &trimmed[digits_end..];
            let Some(marker) = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')')) else {
                return false;
            };
            marker.is_empty() || marker.starts_with(char::is_whitespace)
        }
        _ => false,
    }
}

/// A fence marker: the character (`` ` `` or `~`) and how many of it opened
/// the fence. A closing fence needs at least this many of the same
/// character, per CommonMark.
type Fence = (char, usize);

fn detect_fence_open(line: &str) -> Option<Fence> {
    let trimmed = line.trim_start();
    let indent = line.chars().count() - trimmed.chars().count();
    if indent > 3 {
        return None;
    }
    let ch = trimmed.chars().next()?;
    if ch != '`' && ch != '~' {
        return None;
    }
    let len = trimmed.chars().take_while(|&c| c == ch).count();
    if len < 3 {
        return None;
    }
    // A backtick fence's info string may not itself contain a backtick -
    // that is how CommonMark tells `` `code` `` apart from a real fence.
    let info = &trimmed[trimmed
        .char_indices()
        .nth(len)
        .map_or(trimmed.len(), |(i, _)| i)..];
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, len))
}

fn is_closing_fence(line: &str, fence: Fence) -> bool {
    let (fence_char, fence_len) = fence;
    let trimmed = line.trim_start();
    let indent = line.chars().count() - trimmed.chars().count();
    if indent > 3 {
        return false;
    }
    let len = trimmed.chars().take_while(|&c| c == fence_char).count();
    if len < fence_len {
        return false;
    }
    let rest = &trimmed[trimmed
        .char_indices()
        .nth(len)
        .map_or(trimmed.len(), |(i, _)| i)..];
    rest.trim().is_empty()
}

/// Splits incremental Markdown text into blocks safe to render now.
///
/// Feed text as it arrives via [`push`](Self::push); each call returns the
/// blocks that became confirmed. Call [`finish`](Self::finish) once at the
/// end of the reply to flush whatever remains, and
/// [`pending_tail`](Self::pending_tail) at any time for a preview of the
/// unconfirmed remainder (for a "still typing" status line - not meant to be
/// rendered as Markdown itself).
#[derive(Debug, Default)]
pub struct MarkdownBlockSplitter {
    /// Text received but not yet resolved into a complete line.
    partial_line: String,
    /// Lines confirmed to belong to the block being accumulated.
    pending_lines: Vec<String>,
    /// Blank lines seen since the last non-blank pending line, held back
    /// until the next non-blank line (or end of stream) shows whether they
    /// end the block or merely separate two pieces of the same list, table,
    /// or blockquote.
    held_blank_lines: usize,
    /// The open fence, if `pending_lines` currently ends inside one.
    fence: Option<Fence>,
    /// Classification of the last non-blank line appended to `pending_lines`.
    last_kind: Option<LineKind>,
}

impl MarkdownBlockSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed newly arrived text, returning any blocks that are now confirmed.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.partial_line.push_str(delta);
        let mut confirmed = Vec::new();

        while let Some(pos) = self.partial_line.find('\n') {
            let mut line: String = self.partial_line.drain(..=pos).collect();
            line.pop(); // the '\n' itself
            if line.ends_with('\r') {
                line.pop();
            }
            if let Some(block) = self.process_line(&line) {
                confirmed.push(block);
            }
        }

        confirmed
    }

    /// Flush everything remaining and reset to a clean slate - either at the
    /// end of the whole reply, or at an iteration boundary a caller reuses
    /// this splitter across (`StreamSink::finish_iteration`). A pending
    /// block interrupted by the final line (a fence or heading starting
    /// right at the cutoff) can yield two blocks, which is why this returns
    /// a `Vec` rather than one `Option<String>`.
    ///
    /// Resetting `fence` matters even though [`flush`](Self::flush) already
    /// clears `pending_lines`: an *open* fence left over from one iteration
    /// would otherwise make every line of the next iteration look like it is
    /// still inside a code block, since [`process_line`](Self::process_line)
    /// checks `self.fence` before anything else.
    pub fn finish(&mut self) -> Vec<String> {
        let mut confirmed = Vec::new();
        if !self.partial_line.is_empty() {
            let line = std::mem::take(&mut self.partial_line);
            if let Some(block) = self.process_line(&line) {
                confirmed.push(block);
            }
        }
        if let Some(block) = self.flush() {
            confirmed.push(block);
        }
        self.fence = None;
        self.held_blank_lines = 0;
        confirmed
    }

    /// A preview of the text not yet confirmed - for a status line, not for
    /// rendering: it may end mid-line, mid-fence, or mid-list-item.
    pub fn pending_tail(&self) -> String {
        let mut tail = self.pending_lines.join("\n");
        if !tail.is_empty() && !self.partial_line.is_empty() {
            tail.push('\n');
        }
        tail.push_str(&self.partial_line);
        tail
    }

    fn process_line(&mut self, line: &str) -> Option<String> {
        if let Some(fence) = self.fence {
            self.pending_lines.push(line.to_string());
            if is_closing_fence(line, fence) {
                self.fence = None;
                // A closed fence is always block-final: nothing that
                // follows can retroactively belong inside it.
                return self.flush();
            }
            return None;
        }

        if line.trim().is_empty() {
            self.held_blank_lines += 1;
            return None;
        }

        if let Some(open) = detect_fence_open(line) {
            // A fence reliably starts a new block even with no blank line
            // before it (the common "explanation:\n```lang" pattern).
            let flushed = self.flush();
            self.held_blank_lines = 0;
            self.fence = Some(open);
            self.pending_lines.push(line.to_string());
            self.last_kind = None;
            return flushed;
        }

        let kind = classify(line);

        if self.held_blank_lines > 0 {
            let coalesces = self
                .last_kind
                .is_some_and(|prev| coalesces_across_blank(prev, kind));

            if coalesces {
                for _ in 0..self.held_blank_lines {
                    self.pending_lines.push(String::new());
                }
                self.held_blank_lines = 0;
                self.pending_lines.push(line.to_string());
                self.last_kind = Some(kind);
                return self.flush_if_oversized();
            }

            let flushed = self.flush();
            self.held_blank_lines = 0;
            self.pending_lines.push(line.to_string());
            self.last_kind = Some(kind);
            return flushed;
        }

        // No blank line since the last line: a heading still reliably
        // starts a new block (it interrupts a paragraph per CommonMark),
        // purely to keep chunks small - pulldown-cmark would parse it
        // correctly either way if left merged.
        if kind == LineKind::Heading && !self.pending_lines.is_empty() {
            let flushed = self.flush();
            self.pending_lines.push(line.to_string());
            self.last_kind = Some(kind);
            return flushed;
        }

        self.pending_lines.push(line.to_string());
        self.last_kind = Some(kind);
        self.flush_if_oversized()
    }

    fn flush_if_oversized(&mut self) -> Option<String> {
        let len: usize = self.pending_lines.iter().map(|l| l.len() + 1).sum();
        if len > MAX_PENDING_BLOCK_CHARS {
            self.flush()
        } else {
            None
        }
    }

    fn flush(&mut self) -> Option<String> {
        if self.pending_lines.is_empty() {
            return None;
        }
        let text = self.pending_lines.join("\n");
        self.pending_lines.clear();
        self.last_kind = None;
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::super::render_markdown_with_fallback;
    use super::*;

    /// Feed `input` through the splitter one character at a time - the
    /// worst case for chunk boundaries - and return the confirmed blocks.
    fn split_char_by_char(input: &str) -> Vec<String> {
        let mut splitter = MarkdownBlockSplitter::new();
        let mut blocks = Vec::new();
        for ch in input.chars() {
            blocks.extend(splitter.push(&ch.to_string()));
        }
        blocks.extend(splitter.finish());
        blocks
    }

    /// Render `input` as one document, and by splitting + rendering each
    /// block separately and joining with a blank line. These must match:
    /// that agreement is what makes the splitter correct, since
    /// `TerminalRenderer` inserts exactly one blank line between any two
    /// top-level blocks (`ensure_blank_line`) regardless of their kind.
    fn assert_split_matches_whole(input: &str) {
        let whole = render_markdown_with_fallback(input);

        let blocks = split_char_by_char(input);
        assert!(
            !blocks.is_empty(),
            "expected at least one block for {input:?}"
        );
        let rendered: Vec<String> = blocks
            .iter()
            .map(|b| render_markdown_with_fallback(b))
            .collect();
        let reassembled = rendered.join("\n\n");

        assert_eq!(
            reassembled, whole,
            "\n--- input ---\n{input}\n--- blocks ---\n{blocks:#?}"
        );
    }

    #[test]
    fn splits_two_paragraphs_at_the_blank_line() {
        let blocks = split_char_by_char("First paragraph.\n\nSecond paragraph.\n");
        assert_eq!(blocks, vec!["First paragraph.", "Second paragraph."]);
    }

    #[test]
    fn holds_a_fenced_code_block_until_it_closes() {
        let mut splitter = MarkdownBlockSplitter::new();
        assert!(splitter.push("```rust\nfn main() {\n").is_empty());
        assert!(splitter.push("    println!(\"hi\");\n").is_empty());
        let blocks = splitter.push("}\n```\n");
        assert_eq!(
            blocks,
            vec!["```rust\nfn main() {\n    println!(\"hi\");\n}\n```"]
        );
    }

    #[test]
    fn a_fence_interrupts_a_paragraph_with_no_blank_line() {
        let blocks = split_char_by_char("Here it is:\n```sh\nls\n```\n");
        assert_eq!(blocks, vec!["Here it is:", "```sh\nls\n```"]);
    }

    #[test]
    fn tilde_fences_are_recognised() {
        let blocks = split_char_by_char("~~~\ncode\n~~~\n");
        assert_eq!(blocks, vec!["~~~\ncode\n~~~"]);
    }

    #[test]
    fn a_backtick_fence_info_string_may_not_contain_a_backtick() {
        // "`x`" on its own line is inline code inside a paragraph, not a
        // fence - even though it starts with three-or-more backticks it
        // does not, so this checks the boundary is not misdetected.
        assert!(detect_fence_open("``code``").is_none());
    }

    #[test]
    fn a_loose_ordered_list_stays_one_block_across_blank_lines() {
        let input = "1. one\n\n2. two\n\n3. three\n";
        let blocks = split_char_by_char(input);
        assert_eq!(blocks.len(), 1, "blocks: {blocks:#?}");
        assert_split_matches_whole(input);
    }

    #[test]
    fn a_loose_unordered_list_stays_one_block() {
        assert_split_matches_whole("- one\n\n- two\n\n- three\n");
    }

    /// A list is its own block whenever it follows a paragraph, so this
    /// exercises `render_markdown` on a block that begins with a list marker
    /// with nothing before it in that call - unlike the single-block list
    /// tests above, where the "whole" and the "split" render of the list
    /// are the exact same isolated call and could not have caught this.
    #[test]
    fn a_paragraph_followed_by_a_loose_list_round_trips() {
        assert_split_matches_whole("And a loose list:\n\n1. one\n\n2. two\n\n3. three\n");
    }

    #[test]
    fn a_list_item_with_an_indented_continuation_paragraph_stays_together() {
        assert_split_matches_whole("- item one\n\n    continued.\n\n- item two\n");
    }

    #[test]
    fn a_table_stays_one_block_even_with_a_stray_blank_line() {
        let input = "| a | b |\n| - | - |\n| 1 | 2 |\n\n| 3 | 4 |\n";
        assert_split_matches_whole(input);
    }

    #[test]
    fn a_blockquote_stays_one_block_across_blank_lines() {
        assert_split_matches_whole("> line one\n\n> line two\n");
    }

    #[test]
    fn a_paragraph_after_a_list_is_a_new_block() {
        let blocks = split_char_by_char("- one\n- two\n\nBack to prose.\n");
        assert_eq!(blocks, vec!["- one\n- two", "Back to prose."]);
    }

    #[test]
    fn a_heading_interrupts_a_paragraph_with_no_blank_line() {
        let blocks = split_char_by_char("intro\n# Heading\nmore\n");
        assert_eq!(blocks, vec!["intro", "# Heading\nmore"]);
    }

    #[test]
    fn a_setext_heading_needs_no_special_handling() {
        // The underline has no blank line before it, so it is never split
        // away from its paragraph by the blank-line logic - it stays one
        // block, exactly as CommonMark parses it as a heading.
        assert_split_matches_whole("Title\n=====\n\nBody text.\n");
    }

    #[test]
    fn an_unclosed_fence_at_end_of_stream_is_flushed_by_finish() {
        let mut splitter = MarkdownBlockSplitter::new();
        assert!(splitter.push("```rust\nfn main() {}\n").is_empty());
        let blocks = splitter.finish();
        assert_eq!(blocks, vec!["```rust\nfn main() {}"]);
    }

    /// `finish` is also how a caller resets this splitter for reuse between
    /// tool-call iterations (`StreamSink::finish_iteration`). An unclosed
    /// fence left over from one iteration must not make every line of the
    /// next iteration look like it is still inside a code block.
    #[test]
    fn finish_clears_an_open_fence_so_the_splitter_can_be_reused() {
        let mut splitter = MarkdownBlockSplitter::new();
        assert!(splitter.push("```rust\nfn main() {}\n").is_empty());
        assert_eq!(splitter.finish(), vec!["```rust\nfn main() {}"]);

        // Two paragraphs separated by a blank line. If the fence had
        // leaked, `process_line` would still take the "inside a fence"
        // branch - which never treats a blank line as a block boundary -
        // and both paragraphs (plus the blank line between them) would
        // merge into a single flushed block instead of two.
        let mut out = splitter.push("First paragraph.\n\nSecond paragraph.\n");
        out.extend(splitter.finish());
        assert_eq!(out, vec!["First paragraph.", "Second paragraph."]);
    }

    #[test]
    fn finish_flushes_a_trailing_line_with_no_newline() {
        let mut splitter = MarkdownBlockSplitter::new();
        assert!(splitter.push("no trailing newline").is_empty());
        assert_eq!(splitter.finish(), vec!["no trailing newline"]);
    }

    #[test]
    fn finish_can_yield_two_blocks_when_the_final_line_interrupts() {
        let mut splitter = MarkdownBlockSplitter::new();
        assert!(splitter.push("intro\n").is_empty());
        // No trailing newline: this line only resolves inside `finish`.
        let blocks = splitter.push("# Heading");
        assert!(blocks.is_empty());
        assert_eq!(splitter.finish(), vec!["intro", "# Heading"]);
    }

    #[test]
    fn pending_tail_previews_unconfirmed_text() {
        let mut splitter = MarkdownBlockSplitter::new();
        splitter.push("Some text still\ngenerating");
        assert_eq!(splitter.pending_tail(), "Some text still\ngenerating");
    }

    #[test]
    fn a_huge_paragraph_is_flushed_before_it_ends() {
        let mut splitter = MarkdownBlockSplitter::new();
        let long_line = "x".repeat(MAX_PENDING_BLOCK_CHARS + 10);
        let blocks = splitter.push(&format!("{long_line}\n"));
        assert_eq!(blocks, vec![long_line]);
    }

    #[test]
    fn japanese_paragraphs_split_the_same_way() {
        assert_split_matches_whole(
            "これは最初の段落です。\n\nこれは二番目の段落です。コードは以下の通りです。\n```sh\nls -la\n```\n",
        );
    }

    #[test]
    fn a_plain_paragraph_round_trips() {
        assert_split_matches_whole("Just one plain paragraph, nothing special.\n");
    }

    #[test]
    fn headings_and_paragraphs_mixed_round_trip() {
        assert_split_matches_whole(
            "# Title\n\nIntro paragraph.\n\n## Section\n\nMore text here.\n",
        );
    }
}
