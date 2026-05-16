//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `usql`: interactive REPL for UQA.
//!
//! Reads SQL statements terminated by `;` from stdin, runs each
//! through [`uqa_engine::Engine`], and prints the result rows in a
//! plain aligned table. A handful of meta commands round out the
//! REPL: `\q` to quit, `\open <path>` to switch to persistent storage,
//! `\new` to drop back to an in-memory engine, `\help` for the full
//! list. Designed for piped input as well: when stdin is not a terminal,
//! we read every statement until EOF and exit.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::{CmdKind, Highlighter, MatchingBracketHighlighter};
use rustyline::hint::{Hinter, HistoryHinter};
use rustyline::validate::{MatchingBracketValidator, Validator};
use rustyline::{
    CompletionType, Config, Context, EditMode, Editor, Helper, Result as RustylineResult,
};
use uqa_core::Value;
use uqa_engine::migration::{migrate_python_database, PythonMigrationReport};
use uqa_engine::{Engine, SQLResult};
use uqa_graph::GraphStore as _;
use uqa_sql::ast::{ColumnDef, ColumnType, Expr};

const PROMPT_PRIMARY: &str = "usql> ";
const PROMPT_CONTINUATION: &str = "    > ";
const HISTORY_FILE: &str = ".usql_history";
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
    ("\\ds", "Show statistics"),
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
type UsqlEditor = Editor<UsqlHelper, rustyline::history::DefaultHistory>;

struct CompletionEntry {
    name: String,
    kind: &'static str,
}

struct UsqlHelper {
    completion_entries: Vec<CompletionEntry>,
    table_names: BTreeSet<String>,
    foreign_table_names: BTreeSet<String>,
    column_names: BTreeSet<String>,
    highlighter: MatchingBracketHighlighter,
    hinter: HistoryHinter,
    validator: MatchingBracketValidator,
}

impl UsqlHelper {
    fn new(
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

    fn completion_candidates(&self, prefix: &str, after_table_keyword: bool) -> Vec<Pair> {
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

    fn push_keyword_matches(&self, candidates: &mut Vec<Pair>, upper: &str) {
        for entry in &self.completion_entries {
            if entry.name.to_ascii_uppercase().starts_with(upper) {
                candidates.push(Pair {
                    display: format!("{}\t{}", entry.name, entry.kind),
                    replacement: entry.name.clone(),
                });
            }
        }
    }

    fn push_matches(
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

    fn table_completion_candidates(&self, prefix: &str, include_foreign: bool) -> Vec<Pair> {
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
    fn backslash_argument_completions(&self, line: &str) -> Option<(usize, Vec<Pair>)> {
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
            "ds" | "stats" => Some((start, self.table_completion_candidates(prefix, false))),
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

fn backslash_completion_prefix(line: &str) -> Option<(usize, &str)> {
    let trimmed_start = line.len() - line.trim_start().len();
    let prefix = &line[trimmed_start..];
    if prefix.starts_with('\\') && !prefix.chars().any(char::is_whitespace) {
        Some((trimmed_start, prefix))
    } else {
        None
    }
}

fn word_completion_prefix(line: &str) -> (usize, &str) {
    let start = line
        .char_indices()
        .rev()
        .find_map(|(idx, ch)| (!is_completion_char(ch)).then_some(idx + ch.len_utf8()))
        .unwrap_or(0);
    (start, &line[start..])
}

fn is_completion_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn follows_table_keyword(text_before_word: &str) -> bool {
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

fn highlight_sql_line(line: &str) -> String {
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

fn consume_string_literal(
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

fn consume_number(
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

fn consume_word(start: usize, chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>) -> usize {
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

fn is_sql_keyword_or_function(word: &str) -> bool {
    SQL_KEYWORDS
        .iter()
        .any(|keyword| keyword.eq_ignore_ascii_case(word))
        || uqa_sql::registry::is_registered(word)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let action = match CliAction::parse(&args) {
        Ok(action) => action,
        Err(err) => {
            eprintln!("{err}");
            print_usage_stderr();
            return ExitCode::FAILURE;
        }
    };
    match action {
        CliAction::Help => {
            print_usage_stdout();
            ExitCode::SUCCESS
        }
        CliAction::Migrate {
            source,
            destination,
        } => match migrate_python_database(&source, &destination) {
            Ok(report) => {
                print_migration_report_stdout(&report);
                ExitCode::SUCCESS
            }
            Err(err) => {
                eprintln!("migration failed: {err}");
                ExitCode::FAILURE
            }
        },
        CliAction::Run(args) => run_cli(args),
    }
}

fn run_cli(args: CliArgs) -> ExitCode {
    let mut session = match Session::new(args.db_path.clone()) {
        Ok(session) => session,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    if let Some(command) = args.command {
        session.execute_text(&command, &mut out);
        return ExitCode::SUCCESS;
    }

    for script in &args.scripts {
        if let Err(err) = session.run_file(script, &mut out) {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    }

    if !args.scripts.is_empty() && !stdin.is_terminal() {
        return ExitCode::SUCCESS;
    }

    session.run_repl(&mut out)
}

#[derive(Debug)]
enum CliAction {
    Help,
    Migrate {
        source: PathBuf,
        destination: PathBuf,
    },
    Run(CliArgs),
}

#[derive(Debug, Default)]
struct CliArgs {
    db_path: Option<PathBuf>,
    command: Option<String>,
    scripts: Vec<PathBuf>,
}

impl CliAction {
    fn parse(args: &[String]) -> Result<Self, String> {
        match args {
            [cmd] if cmd == "-h" || cmd == "--help" => return Ok(Self::Help),
            [cmd, source, destination]
                if cmd == "migrate-python-db" || cmd == "--migrate-python-db" =>
            {
                return Ok(Self::Migrate {
                    source: PathBuf::from(source),
                    destination: PathBuf::from(destination),
                });
            }
            _ => {}
        }

        let mut parsed = CliArgs::default();
        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--db" => {
                    let Some(path) = args.get(i + 1) else {
                        return Err("--db requires a path".into());
                    };
                    parsed.db_path = Some(PathBuf::from(path));
                    i += 2;
                }
                "-c" => {
                    let Some(command) = args.get(i + 1) else {
                        return Err("-c requires a SQL command string".into());
                    };
                    parsed.command = Some(command.clone());
                    i += 2;
                }
                "-h" | "--help" => return Ok(Self::Help),
                arg if arg.starts_with('-') => return Err(format!("unknown option: {arg}")),
                script => {
                    parsed.scripts.push(PathBuf::from(script));
                    i += 1;
                }
            }
        }
        Ok(Self::Run(parsed))
    }
}

fn print_usage_stdout() {
    println!("{}", usage_text());
}

fn print_usage_stderr() {
    eprintln!("{}", usage_text());
}

fn usage_text() -> &'static str {
    "Usage:\n    usql                        Start with an in-memory database\n    usql --db mydata.db         Start with persistent storage\n    usql script.sql             Execute a SQL script then enter REPL when stdin is a terminal\n    usql --db mydata.db s.sql   Persistent + script\n    usql -c \"SELECT 1\"          Execute a command string and exit\n    usql migrate-python-db <source> <destination>"
}

impl Session {
    fn run_repl(&mut self, out: &mut impl Write) -> ExitCode {
        self.print_banner();
        if io::stdin().is_terminal() {
            return self.run_interactive_repl(out);
        }
        self.run_line_repl(out)
    }

    fn run_interactive_repl(&mut self, out: &mut impl Write) -> ExitCode {
        let config = Config::builder()
            .history_ignore_space(true)
            .completion_type(CompletionType::List)
            .edit_mode(EditMode::Emacs)
            .auto_add_history(true)
            .build();
        let mut editor = match UsqlEditor::with_config(config) {
            Ok(editor) => editor,
            Err(err) => {
                let _ = writeln!(out, "readline init error: {err}");
                return ExitCode::FAILURE;
            }
        };
        self.load_readline_history(&mut editor);
        let mut buffer = String::new();
        loop {
            editor.set_helper(Some(self.repl_helper()));
            let prompt = if buffer.is_empty() {
                PROMPT_PRIMARY
            } else {
                PROMPT_CONTINUATION
            };
            match editor.readline(prompt) {
                Ok(line) => {
                    self.remember_prompt_line(&line);
                    if self
                        .handle_prompt_line_with_history(&line, &mut buffer, out, false)
                        .is_none()
                    {
                        self.append_readline_history(&mut editor);
                        return ExitCode::SUCCESS;
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    buffer.clear();
                    let _ = writeln!(out);
                }
                Err(ReadlineError::Eof) => {
                    let _ = writeln!(out);
                    self.append_readline_history(&mut editor);
                    return ExitCode::SUCCESS;
                }
                Err(err) => {
                    let _ = writeln!(out, "readline error: {err}");
                    self.append_readline_history(&mut editor);
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    fn run_line_repl(&mut self, out: &mut impl Write) -> ExitCode {
        let stdin = io::stdin();
        let mut input = String::new();
        let mut buffer = String::new();
        loop {
            let prompt = if buffer.is_empty() {
                PROMPT_PRIMARY
            } else {
                PROMPT_CONTINUATION
            };
            let _ = write!(out, "{prompt}");
            let _ = out.flush();
            input.clear();
            let read = stdin.lock().read_line(&mut input);
            match read {
                Ok(0) => {
                    let _ = writeln!(out);
                    return ExitCode::SUCCESS;
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = writeln!(out, "stdin error: {e}");
                    return ExitCode::FAILURE;
                }
            }
            let line = input.trim_end_matches(['\n', '\r']);
            if self.handle_prompt_line(line, &mut buffer, out).is_none() {
                return ExitCode::SUCCESS;
            }
        }
    }

    fn handle_prompt_line(
        &mut self,
        line: &str,
        buffer: &mut String,
        out: &mut impl Write,
    ) -> Option<()> {
        self.handle_prompt_line_with_history(line, buffer, out, true)
    }

    fn handle_prompt_line_with_history(
        &mut self,
        line: &str,
        buffer: &mut String,
        out: &mut impl Write,
        record_sql_history: bool,
    ) -> Option<()> {
        if buffer.is_empty() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return Some(());
            }
            if let Some(rest) = trimmed.strip_prefix('\\') {
                if !self.handle_meta(rest, out) {
                    return None;
                }
                return Some(());
            }
        }
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(line);
        if contains_statement_terminator(buffer) {
            self.execute_text_with_history(buffer, out, record_sql_history);
            buffer.clear();
        }
        Some(())
    }
}

struct Session {
    engine: Engine,
    db_path: Option<PathBuf>,
    location: String,
    history: Vec<String>,
    history_path: Option<PathBuf>,
    show_timing: bool,
    expanded: bool,
    output_path: Option<PathBuf>,
}

impl Session {
    fn new(db_path: Option<PathBuf>) -> Result<Self, String> {
        let history_path = history_path();
        let history = history_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|text| {
                text.lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(std::string::ToString::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let (engine, location) = open_engine(db_path.as_deref())?;
        Ok(Self {
            engine,
            db_path,
            location,
            history,
            history_path,
            show_timing: false,
            expanded: false,
            output_path: None,
        })
    }

    fn repl_helper(&self) -> UsqlHelper {
        let mut columns = BTreeSet::new();
        for table in self.engine.table_names() {
            if let Some(defs) = self.engine.describe_table(&table) {
                columns.extend(defs.into_iter().map(|def| def.name));
            }
        }
        let foreign_tables = self.engine.list_foreign_tables();
        for table in &foreign_tables {
            columns.extend(self.engine.foreign_table_columns(table));
        }
        UsqlHelper::new(
            self.engine.table_names(),
            foreign_tables,
            columns.into_iter().collect(),
        )
    }

    fn load_readline_history(&self, editor: &mut UsqlEditor) {
        let Some(path) = &self.history_path else {
            return;
        };
        let _ = editor.load_history(path);
    }

    fn append_readline_history(&self, editor: &mut UsqlEditor) {
        let Some(path) = &self.history_path else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = editor.append_history(path);
    }

    fn remember_prompt_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|entry| entry == trimmed) {
            return;
        }
        self.history.push(trimmed.to_string());
    }

    fn record_statement(&mut self, sql: &str) {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|l| l == trimmed) {
            return;
        }
        self.history.push(trimmed.to_string());
        if let Some(path) = &self.history_path {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{trimmed}");
            }
        }
    }

    fn print_banner(&self) {
        println!(
            "usql {} -- UQA interactive SQL shell",
            env!("CARGO_PKG_VERSION")
        );
        println!("Database: {}", self.location);
        println!("Type SQL statements terminated by ';'");
        println!("Use \\? for help, \\q to quit.");
        println!();
    }

    fn run_file(&mut self, path: &Path, out: &mut impl Write) -> Result<(), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| format!("File not found or unreadable: {}: {err}", path.display()))?;
        self.execute_text(&text, out);
        Ok(())
    }

    fn execute_text(&mut self, text: &str, out: &mut impl Write) {
        self.execute_text_with_history(text, out, true);
    }

    fn execute_text_with_history(
        &mut self,
        text: &str,
        out: &mut impl Write,
        record_history: bool,
    ) {
        for stmt in split_statements(text) {
            if statement_is_pure_comment(&stmt) {
                continue;
            }
            self.run_statement_with_history(&stmt, out, record_history);
        }
    }

    fn run_statement_with_history(
        &mut self,
        sql: &str,
        out: &mut impl Write,
        record_history: bool,
    ) {
        if record_history {
            self.record_statement(sql);
        }
        let start = std::time::Instant::now();
        let outcome = self.engine.sql(sql, &[]);
        let elapsed = start.elapsed();
        match outcome {
            Ok(result) => {
                self.write_query_output(out, |writer| {
                    if self.expanded {
                        print_result_expanded(&result, writer);
                    } else {
                        print_result(&result, writer);
                    }
                });
            }
            Err(err) => {
                let _ = writeln!(out, "ERROR: {err}");
            }
        }
        if self.show_timing {
            let ms = elapsed.as_secs_f64() * 1000.0;
            self.write_query_output(out, |writer| {
                let _ = writeln!(writer, "Time: {ms:.3} ms");
            });
        }
    }

    fn write_query_output(&self, out: &mut impl Write, write: impl FnOnce(&mut dyn Write)) {
        if let Some(path) = &self.output_path {
            match OpenOptions::new().create(true).append(true).open(path) {
                Ok(mut file) => write(&mut file),
                Err(err) => {
                    let _ = writeln!(out, "output failed: {}: {err}", path.display());
                }
            }
        } else {
            write(out);
        }
    }

    /// Returns `false` when the meta command requested an exit.
    #[allow(clippy::too_many_lines)]
    fn handle_meta(&mut self, command: &str, out: &mut impl Write) -> bool {
        let mut parts = command.trim().splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match cmd {
            "q" | "quit" | "exit" => return false,
            "?" | "help" | "h" => print_backslash_help(out),
            "history" => match arg {
                "" => {
                    for line in &self.history {
                        let _ = writeln!(out, "{line}");
                    }
                }
                "clear" => {
                    self.history.clear();
                    if let Some(path) = &self.history_path {
                        let _ = std::fs::remove_file(path);
                    }
                    let _ = writeln!(out, "history cleared");
                }
                other => {
                    let _ = writeln!(out, "usage: \\history [clear] (got {other:?})");
                }
            },
            "open" => {
                if arg.is_empty() {
                    let _ = writeln!(out, "usage: \\open <path>");
                } else {
                    match Engine::open(Path::new(arg)) {
                        Ok(engine) => {
                            self.engine = engine;
                            self.db_path = Some(PathBuf::from(arg));
                            self.location = arg.to_string();
                            let _ = writeln!(out, "opened {arg}");
                        }
                        Err(e) => {
                            let _ = writeln!(out, "open failed: {e}");
                        }
                    }
                }
            }
            "new" => {
                self.engine = Engine::new();
                self.db_path = None;
                self.location = ":memory:".into();
                let _ = writeln!(out, "fresh in-memory engine");
            }
            "reset" => match open_engine(self.db_path.as_deref()) {
                Ok((engine, location)) => {
                    self.engine = engine;
                    self.location = location;
                    let _ = writeln!(out, "Engine reset.");
                }
                Err(err) => {
                    let _ = writeln!(out, "reset failed: {err}");
                }
            },
            "where" => {
                let _ = writeln!(out, "{}", self.location);
            }
            "timing" => {
                self.show_timing = !self.show_timing;
                let state = if self.show_timing { "on" } else { "off" };
                let _ = writeln!(out, "Timing is {state}.");
            }
            "expanded" | "x" => {
                self.expanded = !self.expanded;
                let state = if self.expanded { "on" } else { "off" };
                let _ = writeln!(out, "Expanded display is {state}.");
            }
            "o" => {
                self.handle_output_redirect(arg, out);
            }
            "dt" | "tables" => {
                self.cmd_list_tables(out);
            }
            "describe" | "d" => {
                self.cmd_describe_table(arg, out);
            }
            "di" => {
                self.cmd_list_indexes(out);
            }
            "stats" | "ds" => {
                self.cmd_show_stats(arg, out);
            }
            "dg" | "graphs" => {
                self.cmd_list_graphs(out);
            }
            "dfs" | "dS" => {
                self.cmd_list_foreign_servers(out);
            }
            "dft" | "dF" => {
                self.cmd_list_foreign_tables(out);
            }
            "da" | "analyzers" => {
                let names = self.engine.list_named_analyzers();
                if names.is_empty() {
                    let _ = writeln!(out, "no analyzers registered");
                } else {
                    for name in names {
                        let _ = writeln!(out, "  {name}");
                    }
                }
            }
            "run" => {
                if arg.is_empty() {
                    let _ = writeln!(out, "usage: \\run <file>");
                } else if let Err(err) = self.run_file(Path::new(arg), out) {
                    let _ = writeln!(out, "{err}");
                }
            }
            "migrate-python-db" => {
                self.handle_migrate_python_db(arg, out);
            }
            other => {
                let _ = writeln!(out, "Unknown command: \\{other}");
                print_backslash_help(out);
            }
        }
        true
    }

    fn handle_output_redirect(&mut self, arg: &str, out: &mut impl Write) {
        if arg.is_empty() {
            if let Some(path) = self.output_path.take() {
                let _ = writeln!(out, "Output restored to stdout (was: {}).", path.display());
            } else {
                let _ = writeln!(out, "Output already goes to stdout.");
            }
            return;
        }
        self.output_path = Some(PathBuf::from(arg));
        let _ = writeln!(out, "Output redirected to: {arg}");
    }

    fn cmd_list_tables(&self, out: &mut impl Write) {
        let mut rows = Vec::new();
        for name in self.engine.table_names() {
            let columns = self
                .engine
                .describe_table(&name)
                .map_or(0_i64, |cols| cols.len() as i64);
            rows.push(result_row(vec![
                ("table_name", Value::Str(name.clone())),
                ("type", Value::Str("table".into())),
                ("columns", Value::Int(columns)),
                (
                    "rows",
                    Value::Int(self.engine.table_doc_ids(&name).len() as i64),
                ),
            ]));
        }
        for name in self.engine.list_foreign_tables() {
            if let Some(table) = self.engine.foreign_table(&name) {
                rows.push(result_row(vec![
                    ("table_name", Value::Str(name)),
                    ("type", Value::Str("foreign".into())),
                    ("columns", Value::Int(table.columns.len() as i64)),
                    ("rows", Value::Str(String::new())),
                ]));
            }
        }
        if rows.is_empty() {
            let _ = writeln!(out, "No tables.");
            return;
        }
        print_result(
            &SQLResult::from_rows(
                vec![
                    "table_name".into(),
                    "type".into(),
                    "columns".into(),
                    "rows".into(),
                ],
                rows,
            ),
            out,
        );
    }

    fn cmd_describe_table(&self, name: &str, out: &mut impl Write) {
        if name.is_empty() {
            let _ = writeln!(out, "Usage: \\d <table_name>");
            return;
        }
        if let Some(cols) = self.engine.describe_table(name) {
            let _ = writeln!(out, "Table \"{name}\"");
            print_columns(&cols, out);
            return;
        }
        if let Some(table) = self.engine.foreign_table(name) {
            let _ = writeln!(
                out,
                "Foreign table \"{name}\" (server: {})",
                table.server_name
            );
            let rows = table
                .columns
                .iter()
                .map(|col| {
                    result_row(vec![
                        ("column", Value::Str(col.name.clone())),
                        ("type", Value::Str(fdw_type_name(col.ty).into())),
                        ("constraints", Value::Str(String::new())),
                    ])
                })
                .collect();
            print_result(
                &SQLResult::from_rows(
                    vec!["column".into(), "type".into(), "constraints".into()],
                    rows,
                ),
                out,
            );
            return;
        }
        let _ = writeln!(out, "Table '{name}' does not exist.");
    }

    fn cmd_list_indexes(&self, out: &mut impl Write) {
        if self.engine.table_names().is_empty() {
            let _ = writeln!(out, "No tables.");
            return;
        }
        let mut by_table: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for stat in self.engine.fts_index_stats(None) {
            by_table
                .entry(stat.table_name)
                .or_default()
                .push(stat.field);
        }
        if by_table.is_empty() {
            let _ = writeln!(out, "No indexed fields.");
            return;
        }
        let rows = by_table
            .into_iter()
            .map(|(table, mut fields)| {
                fields.sort();
                result_row(vec![
                    ("table_name", Value::Str(table)),
                    ("indexed_fields", Value::Str(fields.join(", "))),
                ])
            })
            .collect();
        print_result(
            &SQLResult::from_rows(vec!["table_name".into(), "indexed_fields".into()], rows),
            out,
        );
    }

    fn cmd_show_stats(&self, name: &str, out: &mut impl Write) {
        if name.is_empty() {
            let _ = writeln!(out, "Usage: \\ds <table_name>");
            return;
        }
        if self.engine.describe_table(name).is_none() {
            let _ = writeln!(out, "Table '{name}' does not exist.");
            return;
        }
        let stats = self.engine.column_stats(name);
        if stats.is_empty() {
            let _ = writeln!(out, "No statistics for '{name}' (no declared columns).");
            return;
        }
        let row_count = stats.values().next().map_or(0, |s| s.row_count);
        let _ = writeln!(out, "Statistics for \"{name}\" ({row_count} rows)");
        let rows = stats
            .into_iter()
            .map(|(col, s)| {
                result_row(vec![
                    ("column", Value::Str(col)),
                    ("distinct", Value::Int(s.distinct_count as i64)),
                    ("nulls", Value::Int(s.null_count as i64)),
                    ("min", optional_value_to_display_value(s.min_value.as_ref())),
                    ("max", optional_value_to_display_value(s.max_value.as_ref())),
                    ("selectivity", Value::Float(s.equality_selectivity())),
                ])
            })
            .collect();
        print_result(
            &SQLResult::from_rows(
                vec![
                    "column".into(),
                    "distinct".into(),
                    "nulls".into(),
                    "min".into(),
                    "max".into(),
                    "selectivity".into(),
                ],
                rows,
            ),
            out,
        );
    }

    fn cmd_list_foreign_tables(&self, out: &mut impl Write) {
        let names = self.engine.list_foreign_tables();
        if names.is_empty() {
            let _ = writeln!(out, "No foreign tables.");
            return;
        }
        let rows = names
            .into_iter()
            .filter_map(|name| self.engine.foreign_table(&name))
            .map(|table| {
                let options = foreign_table_options_display(&table.options);
                let source = table.options.get("source").cloned().unwrap_or_default();
                result_row(vec![
                    ("table_name", Value::Str(table.name)),
                    ("server", Value::Str(table.server_name)),
                    ("columns", Value::Int(table.columns.len() as i64)),
                    ("source", Value::Str(source)),
                    ("options", Value::Str(options)),
                ])
            })
            .collect();
        print_result(
            &SQLResult::from_rows(
                vec![
                    "table_name".into(),
                    "server".into(),
                    "columns".into(),
                    "source".into(),
                    "options".into(),
                ],
                rows,
            ),
            out,
        );
    }

    fn cmd_list_foreign_servers(&self, out: &mut impl Write) {
        let names = self.engine.list_foreign_servers();
        if names.is_empty() {
            let _ = writeln!(out, "No foreign servers.");
            return;
        }
        let rows = names
            .into_iter()
            .filter_map(|name| self.engine.foreign_server(&name))
            .map(|server| {
                result_row(vec![
                    ("server_name", Value::Str(server.name)),
                    ("fdw_type", Value::Str(server.fdw_type)),
                    ("options", Value::Str(options_display(&server.options))),
                ])
            })
            .collect();
        print_result(
            &SQLResult::from_rows(
                vec!["server_name".into(), "fdw_type".into(), "options".into()],
                rows,
            ),
            out,
        );
    }

    fn cmd_list_graphs(&self, out: &mut impl Write) {
        let names = self.engine.list_graphs();
        if names.is_empty() {
            let _ = writeln!(out, "No named graphs.");
            return;
        }
        let rows = names
            .into_iter()
            .map(|name| {
                let (vertices, edges) = self
                    .engine
                    .graph_with(&name, |store| {
                        (
                            store.vertex_ids_in_graph(&name).len(),
                            store.edges_in_graph(&name).len(),
                        )
                    })
                    .unwrap_or((0, 0));
                result_row(vec![
                    ("graph_name", Value::Str(name)),
                    ("vertices", Value::Int(vertices as i64)),
                    ("edges", Value::Int(edges as i64)),
                ])
            })
            .collect();
        print_result(
            &SQLResult::from_rows(
                vec!["graph_name".into(), "vertices".into(), "edges".into()],
                rows,
            ),
            out,
        );
    }

    fn handle_migrate_python_db(&mut self, arg: &str, out: &mut impl Write) {
        let parts = arg.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 2 {
            let _ = writeln!(out, "usage: \\migrate-python-db <source> <destination>");
            return;
        }
        let source = Path::new(parts[0]);
        let destination = Path::new(parts[1]);
        match migrate_python_database(source, destination) {
            Ok(report) => {
                print_migration_report(&report, out);
                match Engine::open(destination) {
                    Ok(engine) => {
                        self.engine = engine;
                        self.db_path = Some(destination.to_path_buf());
                        self.location = destination.display().to_string();
                        let _ = writeln!(out, "opened {}", self.location);
                    }
                    Err(err) => {
                        let _ = writeln!(out, "open migrated database failed: {err}");
                    }
                }
            }
            Err(err) => {
                let _ = writeln!(out, "migration failed: {err}");
            }
        }
    }
}

fn print_migration_report_stdout(report: &PythonMigrationReport) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    print_migration_report(report, &mut out);
}

fn print_migration_report(report: &PythonMigrationReport, out: &mut impl Write) {
    let _ = writeln!(out, "migrated {}", report.source_path.display());
    let _ = writeln!(out, "destination {}", report.destination_path.display());
    let _ = writeln!(
        out,
        "tables={} documents={} fts_fields={} vector_fields={} indexes={}",
        report.tables, report.documents, report.fts_fields, report.vector_fields, report.indexes
    );
    let _ = writeln!(
        out,
        "graphs={} vertices={} edges={} path_indexes={}",
        report.graphs, report.graph_vertices, report.graph_edges, report.path_indexes
    );
    let _ = writeln!(
        out,
        "analyzers={} table_field_analyzers={} scoring_params={} models={}",
        report.analyzers, report.table_field_analyzers, report.scoring_params, report.models
    );
    let _ = writeln!(
        out,
        "foreign_servers={} foreign_tables={} column_stats={}",
        report.foreign_servers, report.foreign_tables, report.column_stats
    );
}

fn open_engine(db_path: Option<&Path>) -> Result<(Engine, String), String> {
    match db_path {
        Some(path) => Engine::open(path)
            .map(|engine| (engine, path.display().to_string()))
            .map_err(|err| format!("open failed: {}: {err}", path.display())),
        None => Ok((Engine::new(), ":memory:".into())),
    }
}

fn print_backslash_help(out: &mut impl Write) {
    let _ = writeln!(out, "Backslash commands:");
    let _ = writeln!(out, "  \\dt             List tables");
    let _ = writeln!(out, "  \\d  <table>     Describe table schema");
    let _ = writeln!(out, "  \\di             List inverted-index fields");
    let _ = writeln!(out, "  \\dF             List foreign tables");
    let _ = writeln!(out, "  \\dS             List foreign servers");
    let _ = writeln!(out, "  \\dg             List named graphs");
    let _ = writeln!(out, "  \\ds <table>     Show column statistics");
    let _ = writeln!(out, "  \\x              Toggle expanded display");
    let _ = writeln!(out, "  \\o  [file]      Redirect output to file");
    let _ = writeln!(out, "  \\timing         Toggle query timing");
    let _ = writeln!(out, "  \\reset          Reset engine");
    let _ = writeln!(out, "  \\run <file>     Execute SQL from a file");
    let _ = writeln!(out, "  \\open <path>    Switch to persistent storage");
    let _ = writeln!(
        out,
        "  \\new            Switch to a fresh in-memory database"
    );
    let _ = writeln!(out, "  \\where          Show current database");
    let _ = writeln!(out, "  \\q              Quit");
}

fn result_row(entries: Vec<(&str, Value)>) -> BTreeMap<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn print_columns(cols: &[ColumnDef], out: &mut impl Write) {
    let rows = cols
        .iter()
        .map(|col| {
            result_row(vec![
                ("column", Value::Str(col.name.clone())),
                ("type", Value::Str(sql_type_name(&col.ty))),
                ("constraints", Value::Str(column_constraints(col))),
            ])
        })
        .collect();
    print_result(
        &SQLResult::from_rows(
            vec!["column".into(), "type".into(), "constraints".into()],
            rows,
        ),
        out,
    );
}

fn optional_value_to_display_value(value: Option<&Value>) -> Value {
    match value {
        Some(value) => Value::Str(value_to_display(Some(value))),
        None => Value::Str(String::new()),
    }
}

fn sql_type_name(ty: &ColumnType) -> String {
    match ty {
        ColumnType::Integer => "integer".into(),
        ColumnType::Text => "text".into(),
        ColumnType::Real => "real".into(),
        ColumnType::Numeric { precision, scale } => match (precision, scale) {
            (Some(p), Some(s)) => format!("numeric({p},{s})"),
            (Some(p), None) => format!("numeric({p})"),
            _ => "numeric".into(),
        },
        ColumnType::Json => "json".into(),
        ColumnType::Bytea => "bytea".into(),
        ColumnType::Date => "date".into(),
        ColumnType::Time => "time".into(),
        ColumnType::TimeTz => "time with time zone".into(),
        ColumnType::Timestamp => "timestamp".into(),
        ColumnType::TimestampTz => "timestamp with time zone".into(),
        ColumnType::Vector(dim) => format!("vector({dim})"),
    }
}

fn fdw_type_name(ty: uqa_fdw::ColumnType) -> &'static str {
    match ty {
        uqa_fdw::ColumnType::Integer => "integer",
        uqa_fdw::ColumnType::Real => "real",
        uqa_fdw::ColumnType::Text => "text",
        uqa_fdw::ColumnType::Bool => "boolean",
        uqa_fdw::ColumnType::Bytes => "bytea",
    }
}

fn column_constraints(col: &ColumnDef) -> String {
    let mut flags = Vec::new();
    if col.primary_key {
        flags.push("PK".to_string());
    }
    if col.not_null {
        flags.push("NOT NULL".to_string());
    }
    if col.auto_increment {
        flags.push("AUTO".to_string());
    }
    if col.unique {
        flags.push("UNIQUE".to_string());
    }
    if let Some(default) = &col.default {
        flags.push(format!("DEFAULT {}", expr_display(default)));
    }
    if let Some(check) = &col.check {
        flags.push(format!("CHECK ({})", expr_display(check)));
    }
    if let Some(reference) = &col.references {
        flags.push(format!(
            "REFERENCES {}({})",
            reference.table, reference.column
        ));
    }
    flags.join(" ")
}

fn expr_display(expr: &Expr) -> String {
    format!("{expr:?}")
}

fn options_display(options: &BTreeMap<String, String>) -> String {
    options
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn foreign_table_options_display(options: &BTreeMap<String, String>) -> String {
    let mut out = Vec::new();
    if options
        .get("hive_partitioning")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        out.push("hive".to_string());
    }
    out.join(", ")
}

fn split_statements(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_string = false;
    for ch in text.chars() {
        if ch == '\'' {
            in_string = !in_string;
            current.push(ch);
            continue;
        }
        if ch == ';' && !in_string {
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                out.push(trimmed);
            }
            current.clear();
        } else {
            current.push(ch);
        }
    }
    let trailing = current.trim().to_string();
    if !trailing.is_empty() {
        out.push(trailing);
    }
    out
}

fn contains_statement_terminator(text: &str) -> bool {
    let mut in_string = false;
    for ch in text.chars() {
        if ch == '\'' {
            in_string = !in_string;
        } else if ch == ';' && !in_string {
            return true;
        }
    }
    false
}

fn statement_is_pure_comment(statement: &str) -> bool {
    statement
        .lines()
        .all(|line| line.trim().is_empty() || line.trim().starts_with("--"))
}

/// Render the result one column per line per row -- mirrors
/// `PostgreSQL` `psql`'s `\x` expanded display mode.
fn print_result_expanded(result: &SQLResult, out: &mut (impl Write + ?Sized)) {
    if result.rows.is_empty() && result.columns.is_empty() {
        if result.affected_rows > 0 {
            let _ = writeln!(out, "{} row(s) affected", result.affected_rows);
        }
        return;
    }
    let columns: Vec<String> = if result.columns.is_empty() {
        let mut keys: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
        for row in &result.rows {
            for k in row.keys() {
                keys.insert(k);
            }
        }
        keys.into_iter().cloned().collect()
    } else {
        result.columns.clone()
    };
    let label_width = columns.iter().map(String::len).max().unwrap_or(0);
    for (idx, row) in result.rows.iter().enumerate() {
        let _ = writeln!(out, "-[ RECORD {} ]-", idx + 1);
        for col in &columns {
            let value = value_to_display(row.get(col));
            let _ = writeln!(out, "{col:<label_width$} | {value}");
        }
    }
    let _ = writeln!(out, "({} row(s))", result.rows.len());
}

fn print_result(result: &SQLResult, out: &mut (impl Write + ?Sized)) {
    if result.rows.is_empty() && result.columns.is_empty() {
        if result.affected_rows > 0 {
            let _ = writeln!(out, "{} row(s) affected", result.affected_rows);
        }
        return;
    }
    let columns: Vec<String> = if result.columns.is_empty() {
        let mut keys: std::collections::BTreeSet<&String> = std::collections::BTreeSet::new();
        for row in &result.rows {
            for k in row.keys() {
                keys.insert(k);
            }
        }
        keys.into_iter().cloned().collect()
    } else {
        result.columns.clone()
    };

    // Compute column widths for fixed-width pretty-printing.
    let mut widths: Vec<usize> = columns.iter().map(String::len).collect();
    let stringified_rows: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|row| {
            columns
                .iter()
                .map(|c| value_to_display(row.get(c)))
                .collect()
        })
        .collect();
    for row in &stringified_rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    // Header.
    write_row(out, &columns, &widths);
    let separator: String = widths
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("-+-");
    let _ = writeln!(out, "{separator}");
    for row in &stringified_rows {
        write_row(out, row, &widths);
    }
    let _ = writeln!(out, "({} row(s))", result.rows.len());
}

fn write_row(out: &mut (impl Write + ?Sized), cells: &[String], widths: &[usize]) {
    let line: Vec<String> = cells
        .iter()
        .zip(widths.iter())
        .map(|(c, w)| format!("{c:width$}", c = c, width = *w))
        .collect();
    let _ = writeln!(out, "{}", line.join(" | "));
}

fn history_path() -> Option<PathBuf> {
    // Honour the Rust test override if present; otherwise mirror
    // Python usql's ~/.cognica/uqa/.usql_history default.
    if let Ok(custom) = std::env::var("UQA_HISTORY") {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(
                PathBuf::from(home)
                    .join(".cognica")
                    .join("uqa")
                    .join(HISTORY_FILE),
            );
        }
    }
    None
}

fn value_to_display(v: Option<&Value>) -> String {
    match v {
        Some(Value::Null) | None => "NULL".to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Int(n)) => n.to_string(),
        Some(Value::Float(f)) => format!("{f}"),
        Some(Value::Str(s)) => s.clone(),
        Some(Value::Bytes(b)) => format!("<{} bytes>", b.len()),
        Some(Value::Temporal(t)) => t.to_sql_string(),
        Some(Value::List(items)) => {
            let inner: Vec<String> = items.iter().map(|v| value_to_display(Some(v))).collect();
            format!("[{}]", inner.join(", "))
        }
        Some(Value::Map(m)) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, v)| format!("{k}: {}", value_to_display(Some(v))))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustyline::completion::Candidate as _;
    use rustyline::history::MemHistory;

    #[test]
    fn completion_reads_uqa_function_registry() {
        let helper = UsqlHelper::new(Vec::new(), Vec::new(), Vec::new());
        let history = MemHistory::new();
        let ctx = Context::new(&history);
        let (_start, candidates) = helper.complete("SELECT dee", 10, &ctx).unwrap();
        let replacements = candidates
            .iter()
            .map(rustyline::completion::Candidate::replacement)
            .collect::<Vec<_>>();
        assert!(replacements.contains(&"deep_predict"));
        assert!(replacements.contains(&"deep_learn"));
    }

    #[test]
    fn completion_uses_live_schema_names() {
        let helper = UsqlHelper::new(
            vec!["users".into()],
            vec!["events_ext".into()],
            vec!["user_id".into()],
        );
        let history = MemHistory::new();
        let ctx = Context::new(&history);
        let (_start, from_candidates) = helper.complete("SELECT * FROM us", 16, &ctx).unwrap();
        assert!(from_candidates
            .iter()
            .any(|candidate| candidate.replacement() == "users"));

        let (_start, empty_from_candidates) = helper
            .complete("SELECT * FROM ", "SELECT * FROM ".len(), &ctx)
            .unwrap();
        assert!(empty_from_candidates
            .iter()
            .any(|candidate| candidate.replacement() == "users"));

        let (_start, column_candidates) = helper.complete("SELECT user", 11, &ctx).unwrap();
        assert!(column_candidates
            .iter()
            .any(|candidate| candidate.replacement() == "user_id"));
    }

    #[test]
    fn completion_uses_live_schema_names_for_backslash_table_args() {
        let helper = UsqlHelper::new(
            vec!["users".into()],
            vec!["events_ext".into()],
            vec!["user_id".into()],
        );
        let history = MemHistory::new();
        let ctx = Context::new(&history);

        let (_start, stats_candidates) = helper.complete("\\ds ", "\\ds ".len(), &ctx).unwrap();
        assert!(stats_candidates
            .iter()
            .any(|candidate| candidate.replacement() == "users"));
        assert!(!stats_candidates
            .iter()
            .any(|candidate| candidate.replacement() == "events_ext"));

        let (_start, describe_candidates) =
            helper.complete("\\d ev", "\\d ev".len(), &ctx).unwrap();
        assert!(describe_candidates
            .iter()
            .any(|candidate| candidate.replacement() == "events_ext"));
    }

    #[test]
    fn highlighter_marks_keywords_registry_functions_and_literals() {
        let highlighted = highlight_sql_line("select text_match(body, 'rust') -- comment");
        assert!(highlighted.contains("\x1b[1;34mselect\x1b[0m"));
        assert!(highlighted.contains("\x1b[1;34mtext_match\x1b[0m"));
        assert!(highlighted.contains("\x1b[32m'rust'\x1b[0m"));
        assert!(highlighted.contains("\x1b[90m-- comment\x1b[0m"));
    }

    #[test]
    fn highlighter_forces_refresh_while_typing_sql_tokens() {
        let helper = UsqlHelper::new(Vec::new(), Vec::new(), Vec::new());
        assert!(helper.highlight_char("sele", 4, CmdKind::Other));
        assert!(helper
            .highlight("select", 6)
            .contains("\x1b[1;34mselect\x1b[0m"));
    }

    #[test]
    fn highlighter_keeps_uppercase_keywords_case_insensitive() {
        let highlighted = highlight_sql_line("SELECT text_match(body, 'rust') -- comment");
        assert!(highlighted.contains("\x1b[1;34mSELECT\x1b[0m"));
        assert!(highlighted.contains("\x1b[1;34mtext_match\x1b[0m"));
        assert!(highlighted.contains("\x1b[32m'rust'\x1b[0m"));
        assert!(highlighted.contains("\x1b[90m-- comment\x1b[0m"));
    }
}
