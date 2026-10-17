//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rustyline completion, validation, and SQL syntax highlighting.

use super::{
    BTreeSet, CmdKind, Completer, Context, Cow, Editor, Helper, Highlighter, Hinter, HistoryHinter,
    Pair, RustylineResult, Validator,
};

pub(crate) const SQL_KEYWORDS: &[&str] = &[
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
pub(super) const BACKSLASH_COMMANDS: &[(&str, &str)] = &[
    ("\\dt", "List tables [pattern]"),
    ("\\d", "Describe relations [pattern]"),
    ("\\di", "List indexes [pattern]"),
    ("\\dv", "List views [pattern]"),
    ("\\dn", "List schemas [pattern]"),
    ("\\df", "List routines [pattern]"),
    ("\\du", "List roles [pattern]"),
    ("\\dg", "List roles [pattern]"),
    ("\\da", "List aggregates [pattern]"),
    ("\\dx", "List extensions [pattern]"),
    ("\\dp", "List table privileges [pattern]"),
    ("\\l", "List databases [pattern]"),
    ("\\dft", "List foreign tables"),
    ("\\dfs", "List foreign servers"),
    ("\\dF", "List foreign tables (UQA alias)"),
    ("\\dS", "List system and user relations"),
    ("\\ds", "List sequences [sequence]"),
    ("\\graphs", "List named graphs"),
    ("\\analyzers", "List named analyzers"),
    ("\\stats", "Show statistics <table>"),
    ("\\x", "Expanded display [on|off|auto]"),
    ("\\timing", "Timing [on|off]"),
    ("\\pset", "Set printing options"),
    ("\\a", "Toggle aligned output"),
    ("\\H", "Toggle HTML output"),
    ("\\t", "Tuples only [on|off]"),
    ("\\f", "Set field separator"),
    ("\\C", "Set table title"),
    ("\\o", "Redirect output [file]"),
    ("\\g", "Submit query [file]"),
    ("\\gx", "Submit with expanded output [file]"),
    ("\\gset", "Store result in variables [prefix]"),
    ("\\p", "Print query buffer"),
    ("\\r", "Clear query buffer"),
    ("\\set", "Set a variable"),
    ("\\unset", "Unset a variable"),
    ("\\if", "Begin conditional"),
    ("\\elif", "Conditional alternative"),
    ("\\else", "Conditional alternative"),
    ("\\endif", "End conditional"),
    ("\\echo", "Print arguments"),
    ("\\qecho", "Print to query output"),
    ("\\i", "Include file"),
    ("\\ir", "Include file relative to current script"),
    ("\\run", "Include file"),
    ("\\e", "Edit query [file [line]]"),
    ("\\w", "Write query buffer to file"),
    ("\\s", "Show or save history [file]"),
    ("\\watch", "Repeat query [seconds] [c=count]"),
    ("\\copy", "Copy table/query to or from a local file"),
    ("\\connect", "Connect to database [path]"),
    ("\\conninfo", "Show database connection"),
    ("\\open", "Open database <path>"),
    ("\\new", "New in-memory database"),
    ("\\reset", "Reopen current database"),
    ("\\where", "Show current database"),
    ("\\history", "Show history [clear]"),
    ("\\h", "SQL syntax help [command]"),
    ("\\?", "Backslash help"),
    ("\\help", "Backslash help"),
    ("\\q", "Quit"),
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
    highlighter: crate::highlight::SQLHighlighter,
    hinter: HistoryHinter,
    variables: BTreeSet<String>,
    filename: rustyline::completion::FilenameCompleter,
}

impl UsqlHelper {
    pub(super) fn configure(
        &mut self,
        buffer: &str,
        enabled: bool,
        variables: &crate::variables::Variables,
    ) {
        self.highlighter.set_query_buffer(buffer);
        self.highlighter
            .set_enabled(enabled && std::env::var_os("NO_COLOR").is_none());
        self.variables = variables.entries().map(|(name, _)| name.clone()).collect();
    }
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
            highlighter: crate::highlight::SQLHighlighter::new(),
            hinter: HistoryHinter::new(),
            variables: BTreeSet::new(),
            filename: rustyline::completion::FilenameCompleter::new(),
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
                    display: format!("{}\t{kind}", crate::printing::terminal_text(value)),
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
        ctx: &Context<'_>,
    ) -> RustylineResult<(usize, Vec<Self::Candidate>)> {
        let pos = pos.min(line.len());
        if !line.is_char_boundary(pos) {
            return Ok((pos, Vec::new()));
        }
        let prefix_line = &line[..pos];
        let source = format!("{}{prefix_line}", self.highlighter.prefix());
        let tokens = crate::lexer::tokenize_sql(&source);
        let in_literal = tokens.last().is_some_and(|token| {
            use crate::lexer::TokenKind as K;
            matches!(token.kind, K::String | K::Comment)
                && (!token.closed || token.kind == K::Comment)
        });
        let meta = tokens.iter().any(|token| {
            token.kind == crate::lexer::TokenKind::Meta
                && token.span.start
                    == self.highlighter.prefix().len() + prefix_line.len()
                        - prefix_line.trim_start().len()
        });
        if meta
            && prefix_line
                .split_whitespace()
                .next()
                .is_some_and(|command| {
                    matches!(
                        command,
                        "\\i" | "\\ir" | "\\run" | "\\o" | "\\w" | "\\s" | "\\e" | "\\open"
                    )
                })
        {
            return self.filename.complete(line, pos, ctx);
        }
        if in_literal {
            return Ok((pos, Vec::new()));
        }
        if let Some(token) = tokens
            .last()
            .filter(|token| token.kind == crate::lexer::TokenKind::Identifier && !token.closed)
        {
            if token.span.start < self.highlighter.prefix().len() {
                return Ok((pos, Vec::new()));
            }
            let prefix = token.text(&source);
            return Ok((
                token.span.start - self.highlighter.prefix().len(),
                self.completion_candidates(prefix, false),
            ));
        }
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
        if prefix_line[..start].ends_with(':') && !prefix_line[..start].ends_with("::") {
            return Ok((
                start,
                self.variables
                    .iter()
                    .filter(|name| name.starts_with(prefix))
                    .map(|name| Pair {
                        display: name.clone(),
                        replacement: name.clone(),
                    })
                    .collect(),
            ));
        }
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
            "d" | "describe" | "dt" | "di" | "dv" | "dp" => {
                Some((start, self.table_completion_candidates(prefix, true)))
            }
            "stats" => Some((start, self.table_completion_candidates(prefix, false))),
            "timing" | "t" => Some((start, literal_candidates(prefix, &["on", "off"]))),
            "x" => Some((start, literal_candidates(prefix, &["on", "off", "auto"]))),
            "set" | "unset" => Some((
                start,
                literal_candidates(
                    prefix,
                    &[
                        "AUTOCOMMIT",
                        "ON_ERROR_STOP",
                        "HIGHLIGHT",
                        "HISTCONTROL",
                        "HISTSIZE",
                    ],
                ),
            )),
            "history" => Some((start, literal_candidates(prefix, &["clear"]))),
            "pset" => {
                let words = args.split_whitespace().collect::<Vec<_>>();
                let choices: &[&str] = if words.len() <= 1 && !args.ends_with(char::is_whitespace) {
                    &[
                        "format",
                        "expanded",
                        "tuples_only",
                        "footer",
                        "pager",
                        "fieldsep",
                        "recordsep",
                        "null",
                        "title",
                        "border",
                        "columns",
                        "csv_fieldsep",
                    ]
                } else {
                    match words.first().copied() {
                        Some("format") => &["aligned", "unaligned", "csv", "html", "json"],
                        Some("expanded") => &["on", "off", "auto"],
                        Some("pager") => &["on", "off", "always"],
                        Some("footer" | "tuples_only") => &["on", "off"],
                        _ => &[],
                    }
                };
                Some((start, literal_candidates(prefix, choices)))
            }
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

impl Validator for UsqlHelper {}

impl Highlighter for UsqlHelper {
    fn highlight<'l>(&self, line: &'l str, pos: usize) -> Cow<'l, str> {
        self.highlighter.highlight(line, pos)
    }

    fn highlight_prompt<'b, 's: 'b, 'p: 'b>(
        &'s self,
        prompt: &'p str,
        _default: bool,
    ) -> Cow<'b, str> {
        if self.highlighter.enabled() {
            Cow::Owned(format!("\x1b[1;36m{prompt}\x1b[0m"))
        } else {
            Cow::Borrowed(prompt)
        }
    }

    fn highlight_hint<'h>(&self, hint: &'h str) -> Cow<'h, str> {
        if self.highlighter.enabled() {
            Cow::Owned(format!("\x1b[90m{hint}\x1b[0m"))
        } else {
            Cow::Borrowed(hint)
        }
    }

    fn highlight_char(&self, _line: &str, _pos: usize, _kind: CmdKind) -> bool {
        self.highlighter.enabled()
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
    ch.is_alphanumeric() || matches!(ch, '_' | '.' | '$')
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
        "FROM" | "INTO" | "TABLE" | "ANALYZE" | "JOIN" | "UPDATE" | "REFERENCES"
    )
}

#[cfg(test)]
pub(super) fn highlight_sql_line(line: &str) -> String {
    let mut highlighter = crate::highlight::SQLHighlighter::new();
    highlighter.set_enabled(true);
    highlighter.highlight(line, line.len()).into_owned()
}

fn literal_candidates(prefix: &str, values: &[&str]) -> Vec<Pair> {
    values
        .iter()
        .filter(|value| value.starts_with(prefix))
        .map(|value| Pair {
            display: (*value).into(),
            replacement: (*value).into(),
        })
        .collect()
}
