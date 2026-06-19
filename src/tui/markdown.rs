//! A small, dependency-free Markdown renderer for agent messages.
//!
//! Codex renders assistant output as Markdown; we mirror the common cases —
//! headings, bold, inline code, fenced code blocks, bullet/quote blocks — with
//! a lightweight, width-aware renderer that produces styled ratatui lines.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Render Markdown `text` to styled lines wrapped to `width`.
pub(crate) fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut in_fence = false;

    for raw in text.split('\n') {
        let trimmed = raw.trim_start();

        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue; // never render the fence marker itself
        }
        if in_fence {
            out.push(code_line(raw, width));
            continue;
        }
        if raw.trim().is_empty() {
            out.push(Line::raw(""));
            continue;
        }
        if let Some(level) = heading_level(trimmed) {
            let body = trimmed[level..].trim_start();
            let color = if level <= 2 { Color::Cyan } else { Color::Blue };
            let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
            for line in wrap(body, width) {
                out.push(Line::from(Span::styled(line, style)));
            }
            continue;
        }
        if let Some(quote) = trimmed.strip_prefix('>') {
            let inner = quote.trim_start();
            for line in wrap(inner, width.saturating_sub(2).max(1)) {
                let mut spans = vec![Span::styled("> ", Style::default().fg(Color::DarkGray))];
                spans.extend(inline_spans(&line, Style::default().fg(Color::Gray)));
                out.push(Line::from(spans));
            }
            continue;
        }
        if let Some(item) = bullet_item(trimmed) {
            let inner_width = width.saturating_sub(2).max(1);
            for (i, line) in wrap(item, inner_width).into_iter().enumerate() {
                let marker = if i == 0 { "• " } else { "  " };
                let mut spans = vec![Span::styled(marker, Style::default().fg(Color::Cyan))];
                spans.extend(inline_spans(&line, Style::default()));
                out.push(Line::from(spans));
            }
            continue;
        }
        // Normal paragraph (numbered lists fall through and keep their marker).
        for line in wrap(raw, width) {
            out.push(Line::from(inline_spans(&line, Style::default())));
        }
    }

    out
}

fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && line.chars().nth(hashes) == Some(' ') {
        Some(hashes)
    } else {
        None
    }
}

fn bullet_item(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest);
        }
    }
    None
}

fn code_line(raw: &str, width: usize) -> Line<'static> {
    let body: String = raw.chars().take(width.saturating_sub(2)).collect();
    let mut spans = vec![Span::styled("  ", Style::default())];
    spans.extend(highlight_code(&body));
    Line::from(spans)
}

fn highlight_code(body: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = body.chars().collect();
    let mut i = 0;

    let mut current_text = String::new();
    let mut current_style = Style::default();

    let flush = |text: &mut String, style: Style, spans: &mut Vec<Span<'static>>| {
        if !text.is_empty() {
            spans.push(Span::styled(std::mem::take(text), style));
        }
    };

    while i < chars.len() {
        // 1. Comments
        if (chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/') || chars[i] == '#' {
            flush(&mut current_text, current_style, &mut spans);
            let comment_text: String = chars[i..].iter().collect();
            spans.push(Span::styled(
                comment_text,
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ));
            break;
        }

        // 2. Strings
        if chars[i] == '"' || chars[i] == '\'' {
            flush(&mut current_text, current_style, &mut spans);
            let quote_char = chars[i];
            let mut s = String::new();
            s.push(quote_char);
            i += 1;
            let mut escaped = false;
            while i < chars.len() {
                let c = chars[i];
                s.push(c);
                i += 1;
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == quote_char {
                    break;
                }
            }
            spans.push(Span::styled(s, Style::default().fg(Color::Green)));
            continue;
        }

        // 3. Numbers
        if chars[i].is_ascii_digit() {
            flush(&mut current_text, current_style, &mut spans);
            let mut n = String::new();
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                n.push(chars[i]);
                i += 1;
            }
            spans.push(Span::styled(n, Style::default().fg(Color::Magenta)));
            continue;
        }

        // 4. Identifiers
        if chars[i].is_ascii_alphabetic() || chars[i] == '_' {
            flush(&mut current_text, current_style, &mut spans);
            let mut id = String::new();
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                id.push(chars[i]);
                i += 1;
            }

            let mut is_func = false;
            let mut j = i;
            if j < chars.len() && chars[j] == '!' {
                j += 1;
            }
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if (j < chars.len() && chars[j] == '(')
                || (j + 2 < chars.len()
                    && chars[j] == ':'
                    && chars[j + 1] == ':'
                    && chars[j + 2] == '<')
            {
                is_func = true;
            }

            let style = if is_keyword(&id) {
                Style::default().fg(Color::Yellow)
            } else if is_type(&id) {
                Style::default().fg(Color::Blue)
            } else if is_func {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            spans.push(Span::styled(id, style));
            continue;
        }

        // 5. Punctuation / whitespace
        let c = chars[i];
        i += 1;
        if current_style == Style::default() {
            current_text.push(c);
        } else {
            flush(&mut current_text, current_style, &mut spans);
            current_style = Style::default();
            current_text.push(c);
        }
    }

    flush(&mut current_text, current_style, &mut spans);
    spans
}

fn is_keyword(s: &str) -> bool {
    matches!(
        s,
        "fn" | "let"
            | "mut"
            | "pub"
            | "use"
            | "struct"
            | "impl"
            | "enum"
            | "class"
            | "def"
            | "import"
            | "from"
            | "return"
            | "if"
            | "else"
            | "for"
            | "while"
            | "in"
            | "match"
            | "const"
            | "static"
            | "true"
            | "false"
            | "nil"
            | "null"
            | "None"
            | "self"
            | "Self"
            | "break"
            | "continue"
            | "async"
            | "await"
            | "crate"
            | "mod"
            | "trait"
            | "type"
            | "var"
            | "function"
            | "new"
            | "this"
            | "super"
    )
}

fn is_type(s: &str) -> bool {
    if matches!(
        s,
        "i32"
            | "u32"
            | "i64"
            | "u64"
            | "usize"
            | "isize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "str"
            | "int"
            | "float"
            | "double"
            | "void"
            | "string"
            | "boolean"
            | "object"
            | "any"
    ) {
        return true;
    }
    if let Some(first_char) = s.chars().next()
        && first_char.is_ascii_uppercase()
    {
        return true;
    }
    false
}

/// Parse inline `**bold**` and `` `code` `` spans within one already-wrapped line.
fn inline_spans(text: &str, base: Style) -> Vec<Span<'static>> {
    let code_style = base.fg(Color::Yellow);
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '`' {
            flush(&mut buf, &mut spans, span_style(base, bold));
            i += 1;
            let mut code = String::new();
            while i < chars.len() && chars[i] != '`' {
                code.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                i += 1; // closing backtick
            }
            if !code.is_empty() {
                spans.push(Span::styled(code, code_style));
            }
        } else if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut buf, &mut spans, span_style(base, bold));
            bold = !bold;
            i += 2;
        } else {
            buf.push(chars[i]);
            i += 1;
        }
    }
    flush(&mut buf, &mut spans, span_style(base, bold));
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn flush(buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), style));
    }
}

fn span_style(base: Style, bold: bool) -> Style {
    if bold {
        base.add_modifier(Modifier::BOLD)
    } else {
        base
    }
}

/// Greedy word-wrap that hard-breaks words longer than `width`. Preserves blank
/// lines. Shared with the plain-text cells.
pub(crate) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let mut line = String::new();
        let mut len = 0usize;
        for word in raw.split_whitespace() {
            let wlen = word.chars().count();
            if wlen > width {
                if len > 0 {
                    out.push(std::mem::take(&mut line));
                }
                let mut chunk = String::new();
                let mut clen = 0usize;
                for ch in word.chars() {
                    if clen == width {
                        out.push(std::mem::take(&mut chunk));
                        clen = 0;
                    }
                    chunk.push(ch);
                    clen += 1;
                }
                line = chunk;
                len = clen;
            } else if len == 0 {
                line = word.to_string();
                len = wlen;
            } else if len + 1 + wlen <= width {
                line.push(' ');
                line.push_str(word);
                len += 1 + wlen;
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
                len = wlen;
            }
        }
        out.push(line);
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
    fn fenced_code_block_strips_fences_and_styles_body() {
        let lines = render("text\n```\nlet x = 1;\n```\nmore", 40);
        let t = texts(&lines);
        assert!(t.iter().any(|l| l.contains("let x = 1;")));
        assert!(!t.iter().any(|l| l.contains("```")));
    }

    #[test]
    fn inline_bold_and_code_split_into_spans() {
        let lines = render("a **bold** and `code` end", 80);
        // Spans: "a ", "bold"(bold), " and ", "code"(yellow), " end"
        let spans = &lines[0].spans;
        assert!(
            spans
                .iter()
                .any(|s| s.content.as_ref() == "bold"
                    && s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(spans.iter().any(|s| s.content.as_ref() == "code"));
        // The `**` and backticks are consumed, not shown.
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "a bold and code end");
    }

    #[test]
    fn plain_paragraph_wraps() {
        let lines = render("one two three four", 8);
        assert!(lines.len() >= 2);
    }
}
