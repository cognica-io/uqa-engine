//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rustyline completion, validation, and SQL syntax highlighting.

use super::{
    BTreeSet, CmdKind, Completer, Context, Cow, Editor, Helper, Highlighter, Hinter, HistoryHinter,
    MatchingBracketHighlighter, MatchingBracketValidator, Pair, RustylineResult, Validator,
};

const SQL_KEYWORDS: &[&str] = &[
    "CREATE",
    "TABLE",
    "DROP",
    "IF",
    "EXISTS",
    "PRIMARY",
    "KEY",
    "NOT",
    "NULL",
    "DEFAULT",
    "SERIAL",
    "BIGSERIAL",
    "ALTER",
    "ADD",
    "COLUMN",
    "RENAME",
    "TO",
    "SET",
    "TRUNCATE",
    "UNIQUE",
    "CHECK",
    "CONSTRAINT",
    "INTEGER",
    "INT",
    "BIGINT",
    "SMALLINT",
    "TEXT",
    "VARCHAR",
    "REAL",
    "FLOAT",
    "DOUBLE",
    "PRECISION",
    "NUMERIC",
    "DECIMAL",
    "BOOLEAN",
    "BOOL",
    "CHAR",
    "CHARACTER",
    "JSON",
    "JSONB",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "DELETE",
    "RETURNING",
    "ON",
    "CONFLICT",
    "DO",
    "NOTHING",
    "EXCLUDED",
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "IN",
    "BETWEEN",
    "ORDER",
    "BY",
    "ASC",
    "DESC",
    "LIMIT",
    "OFFSET",
    "AS",
    "DISTINCT",
    "GROUP",
    "HAVING",
    "LIKE",
    "ILIKE",
    "IS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "CAST",
    "COALESCE",
    "NULLIF",
    "UNION",
    "ALL",
    "EXCEPT",
    "INTERSECT",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "CROSS",
    "OUTER",
    "WITH",
    "RECURSIVE",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "ARRAY_AGG",
    "BOOL_AND",
    "BOOL_OR",
    "FILTER",
    "OVER",
    "PARTITION",
    "WINDOW",
    "ROWS",
    "RANGE",
    "UNBOUNDED",
    "PRECEDING",
    "FOLLOWING",
    "CURRENT",
    "ROW",
    "ROW_NUMBER",
    "RANK",
    "DENSE_RANK",
    "NTILE",
    "LAG",
    "LEAD",
    "FIRST_VALUE",
    "LAST_VALUE",
    "NTH_VALUE",
    "PERCENT_RANK",
    "CUME_DIST",
    "SERVER",
    "FOREIGN",
    "DATA",
    "WRAPPER",
    "OPTIONS",
    "IMPORT",
    "EXPLAIN",
    "ANALYZE",
    "GENERATE_SERIES",
];
const BACKSLASH_COMMANDS: &[(&str, &str)] = &[
    ("\\dt", "List tables"),
    ("\\d", "Describe table"),
    ("\\di", "List indexes"),
    ("\\dF", "List foreign tables"),
    ("\\dS", "List foreign servers"),
    ("\\dg", "List graphs"),
    ("\\ds", "List sequences"),
    ("\\stats", "Show column statistics"),
    ("\\x", "Expanded display"),
    ("\\o", "Output to file"),
    ("\\timing", "Toggle timing"),
    ("\\reset", "Reset engine"),
    ("\\q", "Quit"),
    ("\\?", "Help"),
    ("\\help", "Help"),
    ("\\history", "History"),
    ("\\open", "Open database"),
    ("\\new", "New in-memory database"),
    ("\\where", "Show current database"),
    ("\\run", "Run SQL file"),
    ("\\migrate-python-db", "Migrate Python DB"),
];
pub(super) type UsqlEditor = Editor<UsqlHelper, rustyline::history::DefaultHistory>;

pub(super) struct CompletionEntry {
    name: String,
    kind: &'static str,
}

pub(super) struct UsqlHelper {
    completion_entries: Vec<CompletionEntry>,
    table_names: BTreeSet<String>,
    foreign_table_names: BTreeSet<String>,
    column_names: BTreeSet<String>,
    highlighter: MatchingBracketHighlighter,
    hinter: HistoryHinter,
    validator: MatchingBracketValidator,
}

impl UsqlHelper {
    pub(super) fn new(
        table_names: Vec<String>,
        foreign_table_names: Vec<String>,
        column_names: Vec<String>,
    ) -> Self {
        let mut completion_entries = SQL_KEYWORDS
            .iter()
            .map(|name| CompletionEntry {
                name: (*name).to_string(),
                kind: "keyword",
            })
            .collect::<Vec<_>>();
        completion_entries.extend(
            uqa_sql::registry::registered_names()
                .into_iter()
                .map(|name| CompletionEntry {
                    name: name.to_string(),
                    kind: "function",
                }),
        );
        completion_entries.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        });
        completion_entries.dedup_by(|left, right| left.name.eq_ignore_ascii_case(&right.name));
        Self {
            completion_entries,
            table_names: table_names.into_iter().collect(),
            foreign_table_names: foreign_table_names.into_iter().collect(),
            column_names: column_names.into_iter().collect(),
            highlighter: MatchingBracketHighlighter::new(),
            hinter: HistoryHinter::new(),
            validator: MatchingBracketValidator::new(),
        }
    }

    pub(super) fn completion_candidates(
        &self,
        prefix: &str,
        after_table_keyword: bool,
    ) -> Vec<Pair> {
        let upper = prefix.to_ascii_uppercase();
        let mut candidates = Vec::new();
        if after_table_keyword {
            Self::push_matches(&mut candidates, &self.table_names, &upper, "table");
            Self::push_matches(
                &mut candidates,
                &self.foreign_table_names,
                &upper,
                "foreign table",
            );
        } else {
            self.push_keyword_matches(&mut candidates, &upper);
            Self::push_matches(&mut candidates, &self.column_names, &upper, "column");
            Self::push_matches(&mut candidates, &self.table_names, &upper, "table");
            Self::push_matches(
                &mut candidates,
                &self.foreign_table_names,
                &upper,
                "foreign table",
            );
        }
        candidates
    }

    pub(super) fn push_keyword_matches(&self, candidates: &mut Vec<Pair>, upper: &str) {
        for entry in &self.completion_entries {
            if entry.name.to_ascii_uppercase().starts_with(upper) {
                candidates.push(Pair {
                    display: format!("{}\t{}", entry.name, entry.kind),
                    replacement: entry.name.clone(),
                });
            }
        }
    }

    pub(super) fn push_matches(
        candidates: &mut Vec<Pair>,
        values: &BTreeSet<String>,
        upper: &str,
        kind: &str,
    ) {
        for value in values {
            if value.to_ascii_uppercase().starts_with(upper) {
                candidates.push(Pair {
                    display: format!("{value}\t{kind}"),
                    replacement: value.clone(),
                });
            }
        }
    }

    pub(super) fn table_completion_candidates(
        &self,
        prefix: &str,
        include_foreign: bool,
    ) -> Vec<Pair> {
        let upper = prefix.to_ascii_uppercase();
        let mut candidates = Vec::new();
        Self::push_matches(&mut candidates, &self.table_names, &upper, "table");
        if include_foreign {
            Self::push_matches(
                &mut candidates,
                &self.foreign_table_names,
                &upper,
                "foreign table",
            );
        }
        candidates
    }
}

impl Helper for UsqlHelper {}

impl Completer for UsqlHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> RustylineResult<(usize, Vec<Self::Candidate>)> {
        let prefix_line = &line[..pos.min(line.len())];
        if let Some((start, prefix)) = backslash_completion_prefix(prefix_line) {
            let candidates = BACKSLASH_COMMANDS
                .iter()
                .filter(|(cmd, _)| cmd.starts_with(prefix))
                .map(|(cmd, desc)| Pair {
                    display: format!("{cmd}\t{desc}"),
                    replacement: (*cmd).to_string(),
                })
                .collect();
            return Ok((start, candidates));
        }
        if let Some((start, candidates)) = self.backslash_argument_completions(prefix_line) {
            return Ok((start, candidates));
        }
        let (start, prefix) = word_completion_prefix(prefix_line);
        let after_table_keyword = follows_table_keyword(&prefix_line[..start]);
        if prefix.is_empty() && !after_table_keyword {
            return Ok((start, Vec::new()));
        }
        Ok((
            start,
            self.completion_candidates(prefix, after_table_keyword),
        ))
    }
}

impl UsqlHelper {
    pub(super) fn backslash_argument_completions(&self, line: &str) -> Option<(usize, Vec<Pair>)> {
        let trimmed_start = line.len() - line.trim_start().len();
        let text = &line[trimmed_start..];
        let rest = text.strip_prefix('\\')?;
        let command_len = rest.find(char::is_whitespace)?;
        let command = &rest[..command_len];
        let args_start = trimmed_start + 1 + command_len;
        let args = &line[args_start..];
        let (word_offset, prefix) = word_completion_prefix(args);
        let start = args_start + word_offset;
        match command {
            "d" | "describe" => Some((start, self.table_completion_candidates(prefix, true))),
            "stats" => Some((start, self.table_completion_candidates(prefix, false))),
            _ => Some((start, Vec::new())),
        }
    }
}

impl Hinter for UsqlHelper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, ctx: &Context<'_>) -> Option<Self::Hint> {
        self.hinter.hint(line, pos, ctx)
    }
}

impl Validator for UsqlHelper {
    fn validate(
        &self,
        ctx: &mut rustyline::validate::ValidationContext<'_>,
    ) -> RustylineResult<rustyline::validate::ValidationResult> {
        self.validator.validate(ctx)
    }
}

impl Highlighter for UsqlHelper {
    fn highlight<'l>(&self, line: &'l str, pos: usize) -> Cow<'l, str> {
        let highlighted = highlight_sql_line(line);
        if highlighted == line {
            self.highlighter.highlight(line, pos)
        } else {
            Cow::Owned(highlighted)
        }
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        Cow::Owned(format!("\x1b[1;36m{prompt}\x1b[0m"))
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        Cow::Owned(format!("\x1b[90m{hint}\x1b[0m"))
    }

    fn highlight_char(&self, line: &str, pos: usize, kind: CmdKind) -> bool {
        self.highlighter.highlight_char(line, pos, kind)
            || matches!(kind, CmdKind::Other | CmdKind::ForcedRefresh)
    }
}

pub(super) fn backslash_completion_prefix(line: &str) -> Option<(usize, &str)> {
    let trimmed_start = line.len() - line.trim_start().len();
    let prefix = &line[trimmed_start..];
    if prefix.starts_with('\\') && !prefix.chars().any(char::is_whitespace) {
        Some((trimmed_start, prefix))
    } else {
        None
    }
}

pub(super) fn word_completion_prefix(line: &str) -> (usize, &str) {
    let start = line
        .char_indices()
        .rev()
        .find_map(|(idx, ch)| (!is_completion_char(ch)).then_some(idx + ch.len_utf8()))
        .unwrap_or(0);
    (start, &line[start..])
}

pub(super) fn is_completion_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

pub(super) fn follows_table_keyword(text_before_word: &str) -> bool {
    let previous = text_before_word
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .trim_matches(|ch: char| !is_completion_char(ch))
        .to_ascii_uppercase();
    matches!(
        previous.as_str(),
        "FROM" | "INTO" | "TABLE" | "ANALYZE" | "JOIN"
    )
}

pub(super) fn highlight_sql_line(line: &str) -> String {
    if line.trim_start().starts_with('\\') {
        return format!("\x1b[1;36m{line}\x1b[0m");
    }
    let mut out = String::with_capacity(line.len());
    let mut chars = line.char_indices().peekable();
    while let Some((idx, ch)) = chars.next() {
        if ch == '-' && chars.peek().is_some_and(|(_, next)| *next == '-') {
            out.push_str("\x1b[90m");
            out.push_str(&line[idx..]);
            out.push_str("\x1b[0m");
            break;
        }
        if ch == '\'' {
            let end = consume_string_literal(line, &mut chars);
            out.push_str("\x1b[32m");
            out.push_str(&line[idx..end]);
            out.push_str("\x1b[0m");
            continue;
        }
        if ch.is_ascii_digit() {
            let end = consume_number(idx, &mut chars);
            out.push_str("\x1b[35m");
            out.push_str(&line[idx..end]);
            out.push_str("\x1b[0m");
            continue;
        }
        if is_completion_char(ch) {
            let end = consume_word(idx, &mut chars);
            let word = &line[idx..end];
            if is_sql_keyword_or_function(word) {
                out.push_str("\x1b[1;34m");
                out.push_str(word);
                out.push_str("\x1b[0m");
            } else {
                out.push_str(word);
            }
            continue;
        }
        out.push(ch);
    }
    out
}

pub(super) fn consume_string_literal(
    line: &str,
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> usize {
    let mut end = line.len();
    for (idx, ch) in chars.by_ref() {
        if ch == '\'' {
            end = idx + ch.len_utf8();
            break;
        }
    }
    end
}

pub(super) fn consume_number(
    start: usize,
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> usize {
    let mut end = start;
    while let Some((idx, ch)) = chars.peek().copied() {
        if ch.is_ascii_digit() || ch == '.' {
            end = idx + ch.len_utf8();
            let _ = chars.next();
        } else {
            break;
        }
    }
    end.max(start + 1)
}

pub(super) fn consume_word(
    start: usize,
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
) -> usize {
    let mut end = start;
    while let Some((idx, ch)) = chars.peek().copied() {
        if is_completion_char(ch) {
            end = idx + ch.len_utf8();
            let _ = chars.next();
        } else {
            break;
        }
    }
    end.max(start + 1)
}

pub(super) fn is_sql_keyword_or_function(word: &str) -> bool {
    SQL_KEYWORDS
        .iter()
        .any(|keyword| keyword.eq_ignore_ascii_case(word))
        || uqa_sql::registry::is_registered(word)
}
