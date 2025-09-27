use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use thiserror::Error;

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_BOLD: &str = "\x1b[1m";
const ANSI_ITALIC: &str = "\x1b[3m";
const ANSI_STRIKE: &str = "\x1b[9m";
const ANSI_DIM: &str = "\x1b[2m";
const ANSI_UNDERLINE: &str = "\x1b[4m";

const COLOR_HEADING_1: &str = "\x1b[38;5;81m";
const COLOR_HEADING_2: &str = "\x1b[38;5;110m";
const COLOR_HEADING_3: &str = "\x1b[38;5;39m";
const COLOR_HEADING_4: &str = "\x1b[38;5;153m";
const COLOR_HEADING_5: &str = "\x1b[38;5;189m";
const COLOR_HEADING_6: &str = "\x1b[38;5;189m";
const COLOR_INLINE_CODE: &str = "\x1b[38;5;214m";
const COLOR_CODE_BLOCK_BG: &str = "\x1b[48;5;234m";
const COLOR_CODE_BLOCK_FG: &str = "\x1b[38;5;223m";
const COLOR_BLOCKQUOTE: &str = "\x1b[38;5;244m";
const COLOR_LINK: &str = "\x1b[38;5;75m";
const COLOR_LIST_MARKER: &str = "\x1b[38;5;150m";

#[derive(Debug, Error)]
pub enum MarkdownRenderError {
    #[error("markdown rendering does not support embedded null bytes")]
    ContainsNull,
}

/// Convert Markdown text to ANSI-colored terminal output.
pub fn render_markdown(input: &str) -> Result<String, MarkdownRenderError> {
    if input.contains('\0') {
        return Err(MarkdownRenderError::ContainsNull);
    }

    if input.trim().is_empty() {
        return Ok(String::new());
    }

    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_FOOTNOTES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);

    let parser = Parser::new_ext(input, options);
    let mut renderer = TerminalRenderer::default();

    for event in parser {
        renderer.handle_event(event);
    }

    Ok(renderer.finish())
}

/// Render Markdown and fall back to the original text when rendering fails.
pub fn render_markdown_with_fallback(input: &str) -> String {
    match render_markdown(input) {
        Ok(rendered) if !rendered.is_empty() => rendered,
        _ => input.to_string(),
    }
}

#[derive(Default)]
struct TerminalRenderer {
    buffer: String,
    style_stack: Vec<InlineStyle>,
    list_stack: Vec<ListState>,
    pending_newlines: usize,
    start_of_line: bool,
    wrote_anything: bool,
}

impl TerminalRenderer {
    fn handle_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.push_text(text.as_ref()),
            Event::Code(text) => self.push_inline_code(text.as_ref()),
            Event::InlineMath(math) => self.push_inline_code(math.as_ref()),
            Event::DisplayMath(math) => {
                self.ensure_blank_line();
                self.push_code_block_text(math.as_ref());
            }
            Event::SoftBreak => self.soft_break(),
            Event::HardBreak => self.hard_break(),
            Event::Rule => self.rule(),
            Event::Html(html) => self.push_text(html.as_ref()),
            Event::InlineHtml(html) => self.push_text(html.as_ref()),
            Event::TaskListMarker(checked) => self.task_marker(checked),
            Event::FootnoteReference(name) => self.push_text(&format!("[{name}]")),
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.in_top_level_block() {
                    self.ensure_blank_line();
                } else {
                    self.ensure_newline();
                }
            }
            Tag::Heading { level, .. } => {
                self.ensure_blank_line();
                self.style_stack.push(InlineStyle::Heading(level));
            }
            Tag::BlockQuote(_) => {
                self.ensure_blank_line();
                self.style_stack.push(InlineStyle::BlockQuote);
            }
            Tag::CodeBlock(_) => {
                self.ensure_blank_line();
                self.style_stack.push(InlineStyle::CodeBlock);
            }
            Tag::Emphasis => self.style_stack.push(InlineStyle::Italic),
            Tag::Strong => self.style_stack.push(InlineStyle::Bold),
            Tag::Strikethrough => self.style_stack.push(InlineStyle::Strikethrough),
            Tag::List(start) => {
                self.ensure_blank_line();
                if let Some(initial) = start {
                    self.list_stack.push(ListState::ordered(initial as usize));
                } else {
                    self.list_stack.push(ListState::unordered());
                }
            }
            Tag::Item => self.start_list_item(),
            Tag::Link { dest_url, .. } => {
                let dest = dest_url.into_string();
                self.style_stack.push(InlineStyle::Link(dest));
            }
            Tag::Image { .. } => {}
            Tag::Table(_)
            | Tag::TableHead
            | Tag::TableRow
            | Tag::TableCell
            | Tag::FootnoteDefinition(_) => {
                self.ensure_blank_line();
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.ensure_newline(),
            TagEnd::Heading(_) => {
                self.pop_style(|style| matches!(style, InlineStyle::Heading(_)));
                self.ensure_blank_line();
            }
            TagEnd::BlockQuote(_) => {
                self.pop_style(|style| matches!(style, InlineStyle::BlockQuote));
                self.ensure_blank_line();
            }
            TagEnd::CodeBlock => {
                self.pop_style(|style| matches!(style, InlineStyle::CodeBlock));
                self.ensure_blank_line();
            }
            TagEnd::Emphasis => {
                self.pop_style(|style| matches!(style, InlineStyle::Italic));
            }
            TagEnd::Strong => {
                self.pop_style(|style| matches!(style, InlineStyle::Bold));
            }
            TagEnd::Strikethrough => {
                self.pop_style(|style| matches!(style, InlineStyle::Strikethrough));
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.ensure_newline();
            }
            TagEnd::Item => {
                if !self.start_of_line {
                    self.buffer.push('\n');
                    self.start_of_line = true;
                }
                self.ensure_newline();
            }
            TagEnd::Link => {
                if let Some(InlineStyle::Link(dest)) =
                    self.pop_style(|style| matches!(style, InlineStyle::Link(_)))
                {
                    self.append_link_suffix(&dest);
                }
            }
            TagEnd::Table
            | TagEnd::TableHead
            | TagEnd::TableRow
            | TagEnd::TableCell
            | TagEnd::FootnoteDefinition => {
                self.ensure_blank_line();
            }
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let flags = self.current_style();
        if flags.code_block {
            self.push_code_block_text(text);
            return;
        }

        self.prepare_line();
        let mut remaining = text;

        while let Some(pos) = remaining.find('\n') {
            let (line, rest) = remaining.split_at(pos);
            if !line.is_empty() {
                self.write_styled(line, &flags);
            }
            self.buffer.push('\n');
            self.start_of_line = true;
            self.prepare_line();
            remaining = &rest[1..];
        }

        if !remaining.is_empty() {
            self.write_styled(remaining, &flags);
        }
    }

    fn push_inline_code(&mut self, code: &str) {
        let mut flags = self.current_style();
        flags.inline_code = true;
        self.prepare_line();
        self.write_styled(code, &flags);
    }

    fn push_code_block_text(&mut self, text: &str) {
        self.flush_newlines();
        let quote_prefix = self.quote_prefix();
        let indent = if self.list_stack.is_empty() {
            String::new()
        } else {
            "  ".repeat(self.list_stack.len())
        };

        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                self.buffer.push('\n');
            }
            if !quote_prefix.is_empty() {
                self.buffer.push_str(&quote_prefix);
            }
            if !indent.is_empty() {
                self.buffer.push_str(&indent);
            }
            self.buffer.push_str(COLOR_CODE_BLOCK_BG);
            self.buffer.push_str(COLOR_CODE_BLOCK_FG);
            self.buffer.push(' ');
            self.buffer.push_str(line);
            self.buffer.push(' ');
            self.buffer.push_str(ANSI_RESET);
            self.start_of_line = false;
            self.wrote_anything = true;
        }
        self.ensure_newline();
    }

    fn soft_break(&mut self) {
        self.buffer.push('\n');
        self.start_of_line = true;
        self.prepare_line();
    }

    fn hard_break(&mut self) {
        self.buffer.push('\n');
        self.start_of_line = true;
        self.prepare_line();
    }

    fn rule(&mut self) {
        self.ensure_blank_line();
        self.flush_newlines();
        let line = "─".repeat(40);
        self.buffer.push_str(ANSI_DIM);
        self.buffer.push_str(&line);
        self.buffer.push_str(ANSI_RESET);
        self.buffer.push('\n');
        self.start_of_line = true;
    }

    fn task_marker(&mut self, checked: bool) {
        let marker = if checked { "[x]" } else { "[ ]" };
        self.buffer
            .push_str(&format!("{COLOR_LIST_MARKER}{marker}{ANSI_RESET} "));
        self.start_of_line = false;
    }

    fn start_list_item(&mut self) {
        self.flush_newlines();
        if !self.start_of_line {
            self.buffer.push('\n');
        }
        self.start_of_line = true;

        let prefix = self.quote_prefix();
        if !prefix.is_empty() {
            self.buffer.push_str(&prefix);
        }

        let depth = self.list_stack.len();
        if depth > 1 {
            self.buffer.push_str(&"  ".repeat(depth - 1));
        }

        if let Some(state) = self.list_stack.last_mut() {
            match &mut state.kind {
                ListKind::Unordered => {
                    self.buffer
                        .push_str(&format!("{COLOR_LIST_MARKER}•{ANSI_RESET} "));
                }
                ListKind::Ordered { index } => {
                    let current = *index;
                    *index += 1;
                    self.buffer
                        .push_str(&format!("{COLOR_LIST_MARKER}{current}.{ANSI_RESET} "));
                }
            }
        }
        self.start_of_line = false;
        self.wrote_anything = true;
    }

    fn append_link_suffix(&mut self, dest: &str) {
        if dest.is_empty() {
            return;
        }

        self.buffer.push_str(&format!(" ({dest})"));
        self.start_of_line = false;
        self.wrote_anything = true;
    }

    fn prepare_line(&mut self) {
        self.flush_newlines();
        if self.start_of_line {
            let prefix = self.quote_prefix();
            if !prefix.is_empty() {
                self.buffer.push_str(&prefix);
                self.start_of_line = false;
                self.wrote_anything = true;
            }
        }
    }

    fn flush_newlines(&mut self) {
        if self.pending_newlines == 0 {
            return;
        }
        let count = self.pending_newlines;
        for _ in 0..count {
            if !self.buffer.is_empty() && !self.buffer.ends_with('\n') {
                self.buffer.push('\n');
            } else if self.buffer.is_empty() {
                // skip leading blank lines
            } else {
                self.buffer.push('\n');
            }
        }
        self.pending_newlines = 0;
        self.start_of_line = true;
    }

    fn ensure_newline(&mut self) {
        if self.wrote_anything {
            self.pending_newlines = self.pending_newlines.max(1);
        }
    }

    fn ensure_blank_line(&mut self) {
        if self.wrote_anything {
            self.pending_newlines = self.pending_newlines.max(2);
        }
    }

    fn in_top_level_block(&self) -> bool {
        self.list_stack.is_empty() && self.quote_depth() == 0
    }

    fn write_styled(&mut self, text: &str, flags: &StyleFlags) {
        if text.is_empty() {
            return;
        }

        let mut prefix = String::new();
        let mut needs_reset = false;

        if let Some(level) = flags.heading {
            let color = match level {
                HeadingLevel::H1 => COLOR_HEADING_1,
                HeadingLevel::H2 => COLOR_HEADING_2,
                HeadingLevel::H3 => COLOR_HEADING_3,
                HeadingLevel::H4 => COLOR_HEADING_4,
                HeadingLevel::H5 => COLOR_HEADING_5,
                HeadingLevel::H6 => COLOR_HEADING_6,
            };
            prefix.push_str(color);
            prefix.push_str(ANSI_BOLD);
            if level == HeadingLevel::H1 {
                prefix.push_str(ANSI_UNDERLINE);
            }
        } else {
            if flags.bold {
                prefix.push_str(ANSI_BOLD);
            }
            if flags.italic {
                prefix.push_str(ANSI_ITALIC);
            }
        }

        if flags.strike {
            prefix.push_str(ANSI_STRIKE);
        }
        if flags.inline_code {
            prefix.push_str(COLOR_INLINE_CODE);
        }
        if flags.link {
            prefix.push_str(COLOR_LINK);
            prefix.push_str(ANSI_UNDERLINE);
        }
        if flags.quote && !flags.inline_code {
            prefix.push_str(COLOR_BLOCKQUOTE);
        }

        if !prefix.is_empty() {
            needs_reset = true;
            self.buffer.push_str(&prefix);
        }

        self.buffer.push_str(text);

        if needs_reset {
            self.buffer.push_str(ANSI_RESET);
        }

        self.start_of_line = false;
        self.wrote_anything = true;
    }

    fn quote_depth(&self) -> usize {
        self.style_stack
            .iter()
            .filter(|style| matches!(style, InlineStyle::BlockQuote))
            .count()
    }

    fn quote_prefix(&self) -> String {
        if self.quote_depth() == 0 {
            String::new()
        } else {
            "│ ".repeat(self.quote_depth())
        }
    }

    fn current_style(&self) -> StyleFlags {
        let mut flags = StyleFlags::default();

        for style in &self.style_stack {
            match style {
                InlineStyle::Bold => flags.bold = true,
                InlineStyle::Italic => flags.italic = true,
                InlineStyle::Strikethrough => flags.strike = true,
                InlineStyle::CodeBlock => flags.code_block = true,
                InlineStyle::Heading(level) => flags.heading = Some(*level),
                InlineStyle::Link(_) => flags.link = true,
                InlineStyle::BlockQuote => flags.quote = true,
            }
        }

        flags
    }

    fn pop_style<F>(&mut self, predicate: F) -> Option<InlineStyle>
    where
        F: Fn(&InlineStyle) -> bool,
    {
        if let Some(pos) = self.style_stack.iter().rposition(predicate) {
            Some(self.style_stack.remove(pos))
        } else {
            None
        }
    }

    fn finish(mut self) -> String {
        self.flush_newlines();
        while self.buffer.ends_with('\n') {
            self.buffer.pop();
        }
        self.buffer
    }
}

#[derive(Clone, Debug)]
enum InlineStyle {
    Bold,
    Italic,
    Strikethrough,
    CodeBlock,
    Heading(HeadingLevel),
    Link(String),
    BlockQuote,
}

#[derive(Clone, Debug)]
struct ListState {
    kind: ListKind,
}

impl ListState {
    fn unordered() -> Self {
        Self {
            kind: ListKind::Unordered,
        }
    }

    fn ordered(start: usize) -> Self {
        Self {
            kind: ListKind::Ordered { index: start },
        }
    }
}

#[derive(Clone, Debug)]
enum ListKind {
    Unordered,
    Ordered { index: usize },
}

#[derive(Default)]
struct StyleFlags {
    bold: bool,
    italic: bool,
    strike: bool,
    inline_code: bool,
    code_block: bool,
    heading: Option<HeadingLevel>,
    link: bool,
    quote: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOLD: &str = "\x1b[1m";
    const ITALIC: &str = "\x1b[3m";

    #[test]
    fn renders_headings_and_emphasis() {
        let input = "# Title\n\nThis is *italic* and **bold**.";
        let output = render_markdown(input).expect("markdown render");
        assert!(output.contains("Title"));
        assert!(output.contains(BOLD));
        assert!(output.contains(ITALIC));
    }

    #[test]
    fn renders_lists_and_code_block() {
        let input = "- first\n- second\n\n```\nlet x = 42;\n```";
        let output = render_markdown(input).expect("markdown render");
        assert!(output.contains("•"));
        assert!(output.contains(COLOR_CODE_BLOCK_BG));
        assert!(output.contains(COLOR_CODE_BLOCK_FG));
    }

    #[test]
    fn fallback_returns_original_on_error() {
        let input = "broken\0markdown";
        let output = render_markdown_with_fallback(input);
        assert_eq!(output, input);
    }
}
