//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `usql`: minimal interactive REPL for UQA.
//!
//! Reads SQL statements terminated by `;` from stdin, runs each
//! through [`uqa_engine::Engine`], and prints the result rows in a
//! plain aligned table. A handful of meta commands round out the
//! REPL: `\q` to quit, `\open <path>` to switch to a SQLite-backed
//! engine, `\new` to drop back to an in-memory engine, `\help` for
//! the full list. Designed for piped input as well: when stdin is not
//! a terminal, we read every statement until EOF and exit.

use std::fs::OpenOptions;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use uqa_core::Value;
use uqa_engine::{Engine, SqlResult};

const PROMPT_PRIMARY: &str = "usql> ";
const PROMPT_CONTINUATION: &str = "    > ";
const HISTORY_FILE: &str = ".uqa_history";

fn main() -> ExitCode {
    let mut session = Session::new();
    Session::print_banner();
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();
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
        if buffer.is_empty() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix('\\') {
                if !session.handle_meta(rest, &mut out) {
                    return ExitCode::SUCCESS;
                }
                continue;
            }
        }
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(line);
        if buffer.trim_end().ends_with(';') {
            let stmt = buffer.trim_end_matches(';').trim().to_string();
            buffer.clear();
            if !stmt.is_empty() {
                session.run_statement(&stmt, &mut out);
            }
        }
    }
}

struct Session {
    engine: Engine,
    location: String,
    history: Vec<String>,
    history_path: Option<PathBuf>,
}

impl Session {
    fn new() -> Self {
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
        Self {
            engine: Engine::new(),
            location: "<memory>".into(),
            history,
            history_path,
        }
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
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
                let _ = writeln!(f, "{trimmed}");
            }
        }
    }

    fn print_banner() {
        println!("UQA usql — type \\help for commands, \\q to quit");
    }

    fn run_statement(&mut self, sql: &str, out: &mut impl Write) {
        self.record_statement(sql);
        match self.engine.sql(sql, &[]) {
            Ok(result) => print_result(&result, out),
            Err(err) => {
                let _ = writeln!(out, "error: {err}");
            }
        }
    }

    /// Returns `false` when the meta command requested an exit.
    fn handle_meta(&mut self, command: &str, out: &mut impl Write) -> bool {
        let mut parts = command.trim().splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();
        match cmd {
            "q" | "quit" | "exit" => {
                let _ = writeln!(out, "bye.");
                return false;
            }
            "help" | "h" => {
                let _ = writeln!(
                    out,
                    "  \\q | \\quit | \\exit       quit\n  \\open <path>             switch to SQLite-backed engine at <path>\n  \\new                     drop back to an empty in-memory engine\n  \\where                   show the current engine location\n  \\history                 print the persisted statement history\n  \\history clear           delete the on-disk history file"
                );
            }
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
                self.location = "<memory>".into();
                let _ = writeln!(out, "fresh in-memory engine");
            }
            "where" => {
                let _ = writeln!(out, "{}", self.location);
            }
            other => {
                let _ = writeln!(out, "unknown command: \\{other}. \\help for the list.");
            }
        }
        true
    }
}

fn print_result(result: &SqlResult, out: &mut impl Write) {
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

fn write_row(out: &mut impl Write, cells: &[String], widths: &[usize]) {
    let line: Vec<String> = cells
        .iter()
        .zip(widths.iter())
        .map(|(c, w)| format!("{c:width$}", c = c, width = *w))
        .collect();
    let _ = writeln!(out, "{}", line.join(" | "));
}

fn history_path() -> Option<PathBuf> {
    // Honour XDG-style overrides if present, then HOME.
    if let Ok(custom) = std::env::var("UQA_HISTORY") {
        if !custom.is_empty() {
            return Some(PathBuf::from(custom));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home).join(HISTORY_FILE));
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
