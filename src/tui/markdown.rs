//! Markdown rendering for agent messages, built on real libraries rather than a
//! hand-rolled approximation: [`pulldown_cmark`] parses the document, [`syntect`]
//! highlights fenced code blocks (multi-language, themed), and wrapping is
//! `unicode-width`-aware so CJK and other wide glyphs lay out correctly.
//!
//! The public surface is intentionally small and stable: [`render`] turns
//! markdown into styled `ratatui` lines wrapped to a width, and [`wrap`] is a
//! plain width-aware word wrapper shared by the non-markdown chat cells.

use std::sync::LazyLock;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Default syntaxes and a dark theme, loaded once. `syntect` parsing is backed
/// by `fancy-regex` (pure Rust), so there is no C toolchain dependency.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
    let mut set = ThemeSet::load_defaults();
    set.themes
        .remove("base16-ocean.dark")
        .or_else(|| set.themes.values().next().cloned())
        .expect("syntect ships default themes")
});

/// Render Markdown `text` to styled lines wrapped to `width`.
pub(crate) fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let mut renderer = Renderer::new(width.max(1));
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    for event in Parser::new_ext(text, options) {
        renderer.handle(event);
    }
    renderer.finish()
}

/// A single styled character, the unit the span-aware wrapper works in.
type Unit = (char, Style);

/// Block-level rendering mode for routing `Text` events.
enum Mode {
    Normal,
    Code { lang: Option<String>, buf: String },
    Table(Table),
}

struct Table {
    alignments: Vec<pulldown_cmark::Alignment>,
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
    head_rows: usize,
    in_head: bool,
}

struct Renderer {
    width: usize,
    out: Vec<Line<'static>>,
    cur: Vec<Unit>,
    mode: Mode,
    bold: bool,
    italic: bool,
    strike: bool,
    link: bool,
    heading: Option<HeadingLevel>,
    /// One entry per open list; `Some(n)` is the next ordered number.
    lists: Vec<Option<u64>>,
    quote_depth: usize,
    /// First-line prefix for the current list item (consumed on first flush).
    pending_prefix: Option<String>,
}

impl Renderer {
    fn new(width: usize) -> Self {
        Self {
            width,
            out: Vec::new(),
            cur: Vec::new(),
            mode: Mode::Normal,
            bold: false,
            italic: false,
            strike: false,
            link: false,
            heading: None,
            lists: Vec::new(),
            quote_depth: 0,
            pending_prefix: None,
        }
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if !self.cur.is_empty() {
            self.flush_block();
        }
        self.out
    }

    fn handle(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => {
                let style = self.inline_style().fg(Color::Yellow);
                self.push_units(&code, style);
            }
            Event::SoftBreak => self.push_units(" ", self.inline_style()),
            Event::HardBreak => self.flush_block(),
            Event::Rule => {
                self.block_separator();
                self.out.push(Line::from(Span::styled(
                    "─".repeat(self.width.min(40)),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            // Raw HTML / footnotes / math: surface their text verbatim.
            Event::Html(s)
            | Event::InlineHtml(s)
            | Event::InlineMath(s)
            | Event::DisplayMath(s) => {
                self.push_units(&s, self.inline_style());
            }
            Event::TaskListMarker(done) => {
                let mark = if done { "[x] " } else { "[ ] " };
                self.push_units(mark, self.inline_style());
            }
            Event::FootnoteReference(name) => {
                self.push_units(&format!("[^{name}]"), self.inline_style());
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.block_separator(),
            Tag::Heading { level, .. } => {
                self.block_separator();
                self.heading = Some(level);
            }
            Tag::BlockQuote(_) => {
                self.block_separator();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(kind) => {
                self.block_separator();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        let token = info.split_whitespace().next().unwrap_or("");
                        (!token.is_empty()).then(|| token.to_string())
                    }
                    CodeBlockKind::Indented => None,
                };
                self.mode = Mode::Code {
                    lang,
                    buf: String::new(),
                };
            }
            Tag::List(start) => {
                // Flush a tight parent item's text before descending, so its
                // line isn't merged with the nested item's content.
                if !self.cur.is_empty() {
                    self.flush_block();
                }
                if self.lists.is_empty() {
                    self.block_separator();
                }
                self.lists.push(start);
            }
            Tag::Item => {
                let depth = self.lists.len();
                let base = "  ".repeat(depth.saturating_sub(1));
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let m = format!("{base}{n}. ");
                        *n += 1;
                        m
                    }
                    _ => format!("{base}• "),
                };
                self.pending_prefix = Some(marker);
            }
            Tag::Strong => self.bold = true,
            Tag::Emphasis => self.italic = true,
            Tag::Strikethrough => self.strike = true,
            Tag::Link { .. } => self.link = true,
            Tag::Table(alignments) => {
                self.block_separator();
                self.mode = Mode::Table(Table {
                    alignments,
                    rows: Vec::new(),
                    row: Vec::new(),
                    cell: String::new(),
                    head_rows: 0,
                    in_head: false,
                });
            }
            Tag::TableHead => {
                if let Mode::Table(t) = &mut self.mode {
                    t.in_head = true;
                }
            }
            Tag::TableRow => {}
            Tag::TableCell => {
                if let Mode::Table(t) = &mut self.mode {
                    t.cell.clear();
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush_block(),
            TagEnd::Heading(_) => {
                self.flush_block();
                self.heading = None;
            }
            TagEnd::BlockQuote(_) => self.quote_depth = self.quote_depth.saturating_sub(1),
            TagEnd::CodeBlock => self.flush_code(),
            TagEnd::List(_) => {
                self.lists.pop();
                self.pending_prefix = None;
            }
            TagEnd::Item => {
                if !self.cur.is_empty() {
                    self.flush_block();
                }
                self.pending_prefix = None;
            }
            TagEnd::Strong => self.bold = false,
            TagEnd::Emphasis => self.italic = false,
            TagEnd::Strikethrough => self.strike = false,
            TagEnd::Link => self.link = false,
            TagEnd::Table => self.flush_table(),
            TagEnd::TableCell => {
                if let Mode::Table(t) = &mut self.mode {
                    let cell = std::mem::take(&mut t.cell);
                    t.row.push(cell);
                }
            }
            TagEnd::TableHead => {
                if let Mode::Table(t) = &mut self.mode {
                    t.rows.push(std::mem::take(&mut t.row));
                    t.head_rows = t.rows.len();
                    t.in_head = false;
                }
            }
            TagEnd::TableRow => {
                if let Mode::Table(t) = &mut self.mode {
                    t.rows.push(std::mem::take(&mut t.row));
                }
            }
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        match &mut self.mode {
            Mode::Code { buf, .. } => buf.push_str(text),
            Mode::Table(t) => t.cell.push_str(text),
            Mode::Normal => {
                let style = self.inline_style();
                self.push_units(text, style);
            }
        }
    }

    fn push_units(&mut self, text: &str, style: Style) {
        for ch in text.chars() {
            self.cur.push((ch, style));
        }
    }

    /// The active inline style from the open emphasis/heading/link/quote state.
    fn inline_style(&self) -> Style {
        if let Some(level) = self.heading {
            let color = if matches!(level, HeadingLevel::H1 | HeadingLevel::H2) {
                Color::Cyan
            } else {
                Color::Blue
            };
            return Style::default().fg(color).add_modifier(Modifier::BOLD);
        }
        let mut style = Style::default();
        if self.link {
            style = style.fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
        } else if self.quote_depth > 0 {
            style = style.fg(Color::Gray);
        }
        if self.bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if self.italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if self.strike {
            style = style.add_modifier(Modifier::CROSSED_OUT);
        }
        style
    }

    /// Insert a blank line between consecutive top-level blocks.
    fn block_separator(&mut self) {
        if self.lists.is_empty() && self.quote_depth == 0 && !self.out.is_empty() {
            self.out.push(Line::raw(""));
        }
    }

    /// Wrap and emit the accumulated inline content as one block, applying the
    /// current list-item / blockquote prefix.
    fn flush_block(&mut self) {
        let (first, cont) = self.prefixes();
        let prefix_style = if self.quote_depth > 0 {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::Cyan)
        };
        let prefix_width = display_width(&first).max(display_width(&cont));
        let avail = self.width.saturating_sub(prefix_width).max(1);
        let wrapped = wrap_units(&self.cur, avail);
        self.cur.clear();

        if wrapped.is_empty() {
            return;
        }
        for (i, units) in wrapped.into_iter().enumerate() {
            let prefix = if i == 0 { &first } else { &cont };
            let mut spans = Vec::new();
            if !prefix.is_empty() {
                spans.push(Span::styled(prefix.clone(), prefix_style));
            }
            spans.extend(coalesce(units));
            self.out.push(Line::from(spans));
        }
    }

    /// The first-line and continuation prefixes for the current block, combining
    /// any blockquote bar with the pending list-item marker.
    fn prefixes(&mut self) -> (String, String) {
        let quote = "▎ ".repeat(self.quote_depth);
        let (first_marker, cont_marker) = match self.pending_prefix.take() {
            Some(marker) => {
                let indent = " ".repeat(display_width(&marker));
                // A later paragraph in the same item aligns under the text.
                self.pending_prefix = Some(indent.clone());
                (marker, indent)
            }
            None if !self.lists.is_empty() => {
                let indent = "  ".repeat(self.lists.len());
                (indent.clone(), indent)
            }
            None => (String::new(), String::new()),
        };
        (
            format!("{quote}{first_marker}"),
            format!("{quote}{cont_marker}"),
        )
    }

    fn flush_code(&mut self) {
        let Mode::Code { lang, buf } = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        for line in highlight_code(&buf, lang.as_deref()) {
            self.out.push(line);
        }
    }

    fn flush_table(&mut self) {
        let Mode::Table(table) = std::mem::replace(&mut self.mode, Mode::Normal) else {
            return;
        };
        for line in render_table(&table, self.width) {
            self.out.push(line);
        }
    }
}

/// Highlight a fenced code block with syntect, returning indented styled lines.
fn highlight_code(code: &str, lang: Option<&str>) -> Vec<Line<'static>> {
    let syntax = lang
        .and_then(|l| SYNTAXES.find_syntax_by_token(l))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, &THEME);

    let mut out = Vec::new();
    for line in LinesWithEndings::from(code) {
        let ranges = highlighter
            .highlight_line(line, &SYNTAXES)
            .unwrap_or_default();
        let mut spans = vec![Span::styled("  ", Style::default())];
        for (syn, text) in ranges {
            let text = text.trim_end_matches('\n');
            if text.is_empty() {
                continue;
            }
            let mut style = Style::default().fg(syntect_color(syn.foreground));
            if syn.font_style.contains(FontStyle::BOLD) {
                style = style.add_modifier(Modifier::BOLD);
            }
            if syn.font_style.contains(FontStyle::ITALIC) {
                style = style.add_modifier(Modifier::ITALIC);
            }
            spans.push(Span::styled(text.to_string(), style));
        }
        out.push(Line::from(spans));
    }
    out
}

fn syntect_color(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

/// Map a syntect style (color + font flags) to a ratatui style.
fn syntect_style(syn: syntect::highlighting::Style) -> Style {
    let mut style = Style::default().fg(syntect_color(syn.foreground));
    if syn.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if syn.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
}

/// Syntax-highlight a single line of code for a file `extension` (or language
/// token), returning styled spans. Stateless per line — handy for diff views.
pub(crate) fn highlight_code_line(line: &str, extension: &str) -> Vec<Span<'static>> {
    let syntax = SYNTAXES
        .find_syntax_by_extension(extension)
        .or_else(|| SYNTAXES.find_syntax_by_token(extension))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, &THEME);
    match highlighter.highlight_line(line, &SYNTAXES) {
        Ok(ranges) => ranges
            .into_iter()
            .map(|(syn, text)| Span::styled(text.to_string(), syntect_style(syn)))
            .collect(),
        Err(_) => vec![Span::raw(line.to_string())],
    }
}

/// Render a parsed table with per-column width alignment and box-drawing rules.
fn render_table(table: &Table, width: usize) -> Vec<Line<'static>> {
    if table.rows.is_empty() {
        return Vec::new();
    }
    let cols = table.rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    // Column widths, capped so a wide table still fits the available space.
    let cap = (width / cols).max(6);
    let mut col_w = vec![0usize; cols];
    for row in &table.rows {
        for (i, cell) in row.iter().enumerate() {
            col_w[i] = col_w[i].max(display_width(cell.trim()).min(cap));
        }
    }

    let header_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let rule_style = Style::default().fg(Color::DarkGray);
    let mut out = Vec::new();
    for (r, row) in table.rows.iter().enumerate() {
        let mut spans = vec![Span::styled("│ ", rule_style)];
        for (c, w) in col_w.iter().enumerate() {
            let raw = row.get(c).map(|s| s.trim()).unwrap_or("");
            let cell = pad_to(raw, *w, table.alignments.get(c).copied());
            let style = if r < table.head_rows {
                header_style
            } else {
                Style::default().fg(Color::Gray)
            };
            spans.push(Span::styled(cell, style));
            spans.push(Span::styled(" │ ", rule_style));
        }
        out.push(Line::from(spans));
        // Header separator rule after the last head row.
        if r + 1 == table.head_rows {
            let mut sep = String::from("├─");
            for (c, w) in col_w.iter().enumerate() {
                sep.push_str(&"─".repeat(*w));
                sep.push_str(if c + 1 == cols { "─┤" } else { "─┼─" });
            }
            out.push(Line::from(Span::styled(sep, rule_style)));
        }
    }
    out
}

/// Pad (and, if needed, truncate) `text` to display width `w` with alignment.
fn pad_to(text: &str, w: usize, align: Option<pulldown_cmark::Alignment>) -> String {
    use pulldown_cmark::Alignment;
    let truncated = truncate_to_width(text, w);
    let pad = w.saturating_sub(display_width(&truncated));
    match align {
        Some(Alignment::Right) => format!("{}{}", " ".repeat(pad), truncated),
        Some(Alignment::Center) => {
            let left = pad / 2;
            format!(
                "{}{}{}",
                " ".repeat(left),
                truncated,
                " ".repeat(pad - left)
            )
        }
        _ => format!("{}{}", truncated, " ".repeat(pad)),
    }
}

fn truncate_to_width(text: &str, max: usize) -> String {
    if display_width(text) <= max {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let cw = char_width(ch);
        if used + cw > max.saturating_sub(1) {
            out.push('…');
            break;
        }
        out.push(ch);
        used += cw;
    }
    out
}

/// Coalesce consecutive same-style units into spans.
fn coalesce(units: Vec<Unit>) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut buf = String::new();
    let mut cur_style: Option<Style> = None;
    for (ch, style) in units {
        match cur_style {
            Some(s) if s == style => buf.push(ch),
            _ => {
                if let Some(s) = cur_style {
                    spans.push(Span::styled(std::mem::take(&mut buf), s));
                }
                cur_style = Some(style);
                buf.push(ch);
            }
        }
    }
    if let Some(s) = cur_style {
        spans.push(Span::styled(buf, s));
    }
    spans
}

/// Greedy, display-width-aware word wrap over styled units. Words wider than
/// `width` are hard-broken; styles are preserved across wraps.
fn wrap_units(units: &[Unit], width: usize) -> Vec<Vec<Unit>> {
    let words = split_words(units);
    if words.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<Vec<Unit>> = Vec::new();
    let mut line: Vec<Unit> = Vec::new();
    let mut line_w = 0usize;

    for word in words {
        let word_w: usize = word.iter().map(|(c, _)| char_width(*c)).sum();
        if word_w > width {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            let mut chunk: Vec<Unit> = Vec::new();
            let mut chunk_w = 0usize;
            for unit in word {
                let cw = char_width(unit.0);
                if chunk_w + cw > width && !chunk.is_empty() {
                    lines.push(std::mem::take(&mut chunk));
                    chunk_w = 0;
                }
                chunk.push(unit);
                chunk_w += cw;
            }
            line = chunk;
            line_w = chunk_w;
        } else if line.is_empty() {
            line = word;
            line_w = word_w;
        } else if line_w + 1 + word_w <= width {
            line.push((' ', Style::default()));
            line.extend(word);
            line_w += 1 + word_w;
        } else {
            lines.push(std::mem::take(&mut line));
            line = word;
            line_w = word_w;
        }
    }
    lines.push(line);
    lines
}

/// Split units into whitespace-delimited words (dropping the whitespace).
fn split_words(units: &[Unit]) -> Vec<Vec<Unit>> {
    let mut words = Vec::new();
    let mut word: Vec<Unit> = Vec::new();
    for &(ch, style) in units {
        if ch.is_whitespace() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push((ch, style));
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn char_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Width-aware word wrap for plain (unstyled) text, preserving blank lines.
/// Shared with the non-markdown chat cells.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let options = textwrap::Options::new(width)
        .break_words(true)
        .word_splitter(textwrap::WordSplitter::NoHyphenation);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        if raw.trim().is_empty() {
            out.push(String::new());
        } else {
            for line in textwrap::wrap(raw, &options) {
                out.push(line.into_owned());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn wrap_breaks_words_and_long_tokens() {
        assert_eq!(
            wrap("hello world", 5),
            vec!["hello".to_string(), "world".to_string()]
        );
        assert_eq!(
            wrap("abcdefgh", 3),
            vec!["abc".to_string(), "def".to_string(), "gh".to_string()]
        );
    }

    #[test]
    fn wrap_is_unicode_width_aware() {
        // Wide (CJK) glyphs count as width 2, so two of them fill a width-4 line.
        let lines = wrap("你好世界", 4);
        assert_eq!(lines, vec!["你好".to_string(), "世界".to_string()]);
    }

    #[test]
    fn heading_is_detected_and_stripped() {
        let lines = render("## Title here", 40);
        assert_eq!(texts(&lines), vec!["Title here".to_string()]);
        assert!(
            lines[0].spans[0]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }

    #[test]
    fn bullets_get_a_marker() {
        let lines = render("- first\n- second", 40);
        let t = texts(&lines);
        assert_eq!(t, vec!["• first".to_string(), "• second".to_string()]);
    }

    #[test]
    fn ordered_list_numbers_items() {
        let lines = render("1. one\n2. two", 40);
        let t = texts(&lines);
        assert_eq!(t, vec!["1. one".to_string(), "2. two".to_string()]);
    }

    #[test]
    fn nested_bullets_indent() {
        let lines = render("- top\n  - child", 40);
        let t = texts(&lines);
        assert_eq!(t, vec!["• top".to_string(), "  • child".to_string()]);
    }

    #[test]
    fn fenced_code_block_strips_fences_and_styles_body() {
        let lines = render("text\n```rust\nlet x = 1;\n```\nmore", 40);
        let t = texts(&lines);
        assert!(t.iter().any(|l| l.contains("let x = 1;")));
        assert!(!t.iter().any(|l| l.contains("```")));
    }

    #[test]
    fn inline_bold_and_code_split_into_spans() {
        let lines = render("a **bold** and `code` end", 80);
        let spans = &lines[0].spans;
        assert!(
            spans
                .iter()
                .any(|s| s.content.as_ref() == "bold"
                    && s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(spans.iter().any(|s| s.content.as_ref() == "code"));
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "a bold and code end");
    }

    #[test]
    fn plain_paragraph_wraps() {
        let lines = render("one two three four", 8);
        assert!(lines.len() >= 2);
    }

    #[test]
    fn table_renders_with_separators() {
        let lines = render("| a | b |\n|---|---|\n| 1 | 2 |", 40);
        let t = texts(&lines);
        assert!(t.iter().any(|l| l.contains('a') && l.contains('b')));
        assert!(t.iter().any(|l| l.contains('│')));
        assert!(t.iter().any(|l| l.contains('┼')));
    }
}
