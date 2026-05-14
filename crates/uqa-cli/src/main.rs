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
use uqa_engine::migration::{migrate_python_database, PythonMigrationReport};
use uqa_engine::{Engine, SQLResult};

const PROMPT_PRIMARY: &str = "usql> ";
const PROMPT_CONTINUATION: &str = "    > ";
const HISTORY_FILE: &str = ".uqa_history";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        return handle_args(&args);
    }

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

fn handle_args(args: &[String]) -> ExitCode {
    match args {
        [cmd, source, destination]
            if cmd == "migrate-python-db" || cmd == "--migrate-python-db" =>
        {
            match migrate_python_database(Path::new(source), Path::new(destination)) {
                Ok(report) => {
                    print_migration_report_stdout(&report);
                    ExitCode::SUCCESS
                }
                Err(err) => {
                    eprintln!("migration failed: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("usage: usql [migrate-python-db <source> <destination>]");
            ExitCode::FAILURE
        }
    }
}

struct Session {
    engine: Engine,
    location: String,
    history: Vec<String>,
    history_path: Option<PathBuf>,
    show_timing: bool,
    expanded: bool,
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
            show_timing: false,
            expanded: false,
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
        println!("UQA usql - type \\help for commands, \\q to quit");
    }

    fn run_statement(&mut self, sql: &str, out: &mut impl Write) {
        self.record_statement(sql);
        let start = std::time::Instant::now();
        let outcome = self.engine.sql(sql, &[]);
        let elapsed = start.elapsed();
        match outcome {
            Ok(result) => {
                if self.expanded {
                    print_result_expanded(&result, out);
                } else {
                    print_result(&result, out);
                }
            }
            Err(err) => {
                let _ = writeln!(out, "error: {err}");
            }
        }
        if self.show_timing {
            let ms = elapsed.as_secs_f64() * 1000.0;
            let _ = writeln!(out, "Time: {ms:.3} ms");
        }
    }

    /// Returns `false` when the meta command requested an exit.
    #[allow(clippy::too_many_lines)]
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
                    "  \\q | \\quit | \\exit       quit\n  \\open <path>             switch to SQLite-backed engine at <path>\n  \\new                     drop back to an empty in-memory engine\n  \\where                   show the current engine location\n  \\history                 print the persisted statement history\n  \\history clear           delete the on-disk history file\n  \\timing                  toggle per-statement execution timing\n  \\expanded                toggle column-per-line output\n  \\dt                      list registered tables\n  \\describe <table>        show the schema of a table\n  \\stats <table>           show ANALYZE column statistics\n  \\dg | \\graphs            list registered graphs\n  \\dfs                     list registered foreign servers\n  \\dft                     list registered foreign tables\n  \\da | \\analyzers         list registered named analyzers\n  \\migrate-python-db <source> <destination>  migrate a Python UQA SQLite DB\n  \\run <file>              execute SQL from a file"
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
            "timing" => {
                self.show_timing = !self.show_timing;
                let state = if self.show_timing { "on" } else { "off" };
                let _ = writeln!(out, "timing {state}");
            }
            "expanded" | "x" => {
                self.expanded = !self.expanded;
                let state = if self.expanded { "on" } else { "off" };
                let _ = writeln!(out, "expanded display {state}");
            }
            "dt" | "tables" => {
                let names = self.engine.table_names();
                if names.is_empty() {
                    let _ = writeln!(out, "no tables registered");
                } else {
                    for name in names {
                        let _ = writeln!(out, "  {name}");
                    }
                }
            }
            "describe" | "d" => {
                if arg.is_empty() {
                    let _ = writeln!(out, "usage: \\describe <table>");
                } else if let Some(cols) = self.engine.describe_table(arg) {
                    for col in cols {
                        let _ = writeln!(out, "  {}: {:?}", col.name, col.ty);
                    }
                } else {
                    let _ = writeln!(out, "no such table: {arg}");
                }
            }
            "stats" => {
                if arg.is_empty() {
                    let _ = writeln!(out, "usage: \\stats <table>");
                } else {
                    let stats = self.engine.column_stats(arg);
                    if stats.is_empty() {
                        let _ = writeln!(out, "no stats - run ANALYZE {arg}");
                    } else {
                        for (col, s) in stats {
                            let _ = writeln!(
                                out,
                                "  {col}: distinct={} nulls={} min={:?} max={:?}",
                                s.distinct_count, s.null_count, s.min_value, s.max_value
                            );
                        }
                    }
                }
            }
            "dg" | "graphs" => {
                let names = self.engine.list_graphs();
                if names.is_empty() {
                    let _ = writeln!(out, "no graphs registered");
                } else {
                    for name in names {
                        let _ = writeln!(out, "  {name}");
                    }
                }
            }
            "dfs" => {
                let names = self.engine.list_foreign_servers();
                if names.is_empty() {
                    let _ = writeln!(out, "no foreign servers registered");
                } else {
                    for name in names {
                        let _ = writeln!(out, "  {name}");
                    }
                }
            }
            "dft" => {
                let names = self.engine.list_foreign_tables();
                if names.is_empty() {
                    let _ = writeln!(out, "no foreign tables registered");
                } else {
                    for name in names {
                        let _ = writeln!(out, "  {name}");
                    }
                }
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
                } else {
                    match std::fs::read_to_string(arg) {
                        Ok(text) => {
                            for stmt in split_statements(&text) {
                                self.run_statement(&stmt, out);
                            }
                        }
                        Err(e) => {
                            let _ = writeln!(out, "could not read {arg}: {e}");
                        }
                    }
                }
            }
            "migrate-python-db" => {
                self.handle_migrate_python_db(arg, out);
            }
            other => {
                let _ = writeln!(out, "unknown command: \\{other}. \\help for the list.");
            }
        }
        true
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

/// Render the result one column per line per row -- mirrors
/// `PostgreSQL` `psql`'s `\x` expanded display mode.
fn print_result_expanded(result: &SQLResult, out: &mut impl Write) {
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

fn print_result(result: &SQLResult, out: &mut impl Write) {
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
