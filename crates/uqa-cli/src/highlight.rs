//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::borrow::Cow;
use std::fmt::Write;

use crate::completion::SQL_KEYWORDS;
use crate::lexer::{tokenize_sql, TokenKind as Kind};

/// Color incomplete SQL without changing the query text or coloring literal contents as syntax.
pub(crate) struct SQLHighlighter {
    prefix: String,
    enabled: bool,
}

impl SQLHighlighter {
    pub(crate) fn new() -> Self {
        Self {
            prefix: String::new(),
            enabled: std::env::var_os("NO_COLOR").is_none(),
        }
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    pub(crate) const fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn set_query_buffer(&mut self, buffer: &str) {
        self.prefix.clear();
        if !buffer.is_empty() {
            self.prefix.push_str(buffer);
            self.prefix.push('\n');
        }
    }

    pub(crate) fn prefix(&self) -> &str {
        &self.prefix
    }

    pub(crate) fn highlight<'a>(&self, line: &'a str, position: usize) -> Cow<'a, str> {
        if !self.enabled {
            return Cow::Borrowed(line);
        }
        let source = format!("{}{line}", self.prefix);
        let base = self.prefix.len();
        let tokens = tokenize_sql(&source);
        let pair = bracket_pair(&source, &tokens, base + position.min(line.len()));
        let mut result = String::with_capacity(line.len());
        for token in &tokens {
            if token.span.end <= base {
                continue;
            }
            let start = token.span.start.max(base);
            let text = &source[start..token.span.end];
            let style = if pair.is_some_and(|(a, b)| start == a || start == b) {
                Some("1;36")
            } else {
                match token.kind {
                    Kind::Comment => Some("90"),
                    Kind::String => Some("32"),
                    Kind::Identifier => Some("33"),
                    Kind::Number => Some("36"),
                    Kind::Parameter | Kind::Variable => Some("35"),
                    Kind::Meta => Some("1;36"),
                    Kind::Word => word_style(text, &source[token.span.end..]),
                    Kind::Space | Kind::Operator => None,
                }
            };
            if let Some(style) = style {
                write!(result, "\x1b[{style}m{text}\x1b[0m").expect("String writes do not fail");
            } else {
                result.push_str(text);
            }
        }
        Cow::Owned(result)
    }
}

fn word_style(word: &str, rest: &str) -> Option<&'static str> {
    let upper = word.to_ascii_uppercase();
    if SQL_KEYWORDS.contains(&upper.as_str()) {
        return Some("1;34");
    }
    if uqa_sql::registry::lookup(word).is_some() {
        return Some("35");
    }
    rest.trim_start().starts_with('(').then_some("35")
}

fn bracket_pair(
    source: &str,
    tokens: &[crate::lexer::Token],
    position: usize,
) -> Option<(usize, usize)> {
    let brackets = tokens
        .iter()
        .filter_map(|t| {
            if t.kind != Kind::Operator {
                return None;
            }
            let c = t.text(source).chars().next()?;
            matches!(c, '(' | ')' | '[' | ']').then_some((t.span.start, c))
        })
        .collect::<Vec<_>>();
    let index = brackets
        .iter()
        .position(|(p, _)| *p == position)
        .or_else(|| brackets.iter().position(|(p, _)| *p + 1 == position))?;
    let (start, bracket) = brackets[index];
    let opening = matches!(bracket, '(' | '[');
    let mut stack = Vec::new();
    let indices: Box<dyn Iterator<Item = usize>> = if opening {
        Box::new(index..brackets.len())
    } else {
        Box::new((0..=index).rev())
    };
    for i in indices {
        let (p, c) = brackets[i];
        if matches!(c, '(' | '[') == opening {
            stack.push(c);
        } else {
            let previous = stack.pop()?;
            if !matches!(
                (previous, c),
                ('(', ')') | ('[', ']') | (')', '(') | (']', '[')
            ) {
                return None;
            }
            if stack.is_empty() {
                return Some((start, p));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(source: &str) -> String {
        let mut output = String::new();
        let mut chars = source.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                output.push(c);
            }
        }
        output
    }

    #[test]
    fn highlights_keywords_literals_numbers_comments_and_identifiers() {
        let mut highlighter = SQLHighlighter::new();
        highlighter.set_enabled(true);
        let source = "SELECT count(*), 12, 'SELECT', \"name\" -- WHERE";
        let result = highlighter.highlight(source, 0);
        assert!(result.contains("\x1b[1;34mSELECT\x1b[0m"));
        assert!(result.contains("\x1b[32m'SELECT'\x1b[0m"));
        assert!(result.contains("\x1b[90m-- WHERE\x1b[0m"));
        assert_eq!(plain(&result), source);
    }

    #[test]
    fn preserves_multiline_dollar_quote_and_nested_comment_context() {
        let mut highlighter = SQLHighlighter::new();
        highlighter.set_enabled(true);
        for (prefix, line, color) in [
            ("DO $body$ BEGIN", "SELECT 1;", "32"),
            ("/* outer /* nested", "SELECT 1; */ */", "90"),
        ] {
            highlighter.set_query_buffer(prefix);
            let result = highlighter.highlight(line, 0);
            assert!(result.starts_with(&format!("\x1b[{color}m")));
            assert!(!result.contains("\x1b[1;34mSELECT"));
            assert_eq!(plain(&result), line);
        }
    }

    #[test]
    fn brackets_inside_literals_are_not_matched_as_syntax() {
        let source = "SELECT ('[)')";
        let tokens = tokenize_sql(source);
        assert_eq!(bracket_pair(source, &tokens, 7), Some((7, 12)));
        assert_eq!(bracket_pair(source, &tokens, 9), None);
        assert_eq!(bracket_pair("([)]", &tokenize_sql("([)]"), 0), None);
    }

    #[test]
    fn disabled_highlighting_emits_no_ansi_and_preserves_every_unicode_prefix() {
        let mut highlighter = SQLHighlighter::new();
        let source = "SELECT '\u{d55c}\u{ae00}', \"\u{540d}\u{524d}\" /* comment */";
        for enabled in [false, true] {
            highlighter.set_enabled(enabled);
            for end in (0..=source.len()).filter(|i| source.is_char_boundary(*i)) {
                let result = highlighter.highlight(&source[..end], end);
                assert_eq!(plain(&result), &source[..end]);
                if !enabled {
                    assert!(!result.contains('\x1b'));
                }
            }
        }
    }
}
