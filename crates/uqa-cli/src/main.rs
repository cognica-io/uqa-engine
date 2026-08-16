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

struct TrackedWriter<W> {
    inner: W,
    first_error: Option<String>,
}

impl<W> TrackedWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            first_error: None,
        }
    }

    fn error(&self) -> Option<&str> {
        self.first_error.as_deref()
    }

    fn remember_error(&mut self, error: &io::Error) {
        if self.first_error.is_none() {
            self.first_error = Some(error.to_string());
        }
    }
}

impl<W: Write> Write for TrackedWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self.inner.write(buffer) {
            Ok(written) => Ok(written),
            Err(error) => {
                self.remember_error(&error);
                Err(error)
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.inner.flush() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.remember_error(&error);
                Err(error)
            }
        }
    }
}
mod completion;
mod display;
mod meta;
mod migration_io;
mod output;
mod repl;
mod statements;

use completion::{UsqlEditor, UsqlHelper};
use display::{
    fdw_type_name, foreign_table_options_display, optional_value_to_display_value, options_display,
    print_backslash_help, print_columns, result_row, sequence_row, u64_count_value,
    usize_count_value,
};
use migration_io::{
    open_engine, open_engine_with_key, print_migration_report, print_migration_report_stdout,
};
use output::{
    history_path, print_result, print_result_copy_text, print_result_expanded, value_to_display,
};
use repl::{PromptLineOutcome, Session};
use statements::{contains_statement_terminator, split_statements, statement_is_pure_comment};

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
        CliAction::Help => match print_usage_stdout() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("write help output: {error}");
                ExitCode::FAILURE
            }
        },
        CliAction::Migrate {
            source,
            destination,
        } => match migrate_python_database(&source, &destination) {
            Ok(report) => match print_migration_report_stdout(&report) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("write migration report: {error}");
                    ExitCode::FAILURE
                }
            },
            Err(err) => {
                eprintln!("migration failed: {err}");
                ExitCode::FAILURE
            }
        },
        CliAction::Run(args) => run_cli(args),
    }
}

fn run_cli(args: CliArgs) -> ExitCode {
    let key = match args.resolve_key() {
        Ok(key) => key,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    let session = if args.command.is_some() {
        Session::new_without_history(args.db_path.clone(), key.as_deref())
    } else {
        Session::new(args.db_path.clone(), key.as_deref())
    };
    let mut session = match session {
        Ok(session) => session,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };
    session.copy_text = args.copy_text;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = TrackedWriter::new(stdout.lock());

    let exit = if let Some(command) = args.command {
        match session.execute_text_with_history(&command, &mut out, false) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                let _ = writeln!(out, "ERROR: {err}");
                ExitCode::FAILURE
            }
        }
    } else {
        let mut scripts_succeeded = true;
        for script in &args.scripts {
            if let Err(err) = session.run_file(script, &mut out) {
                eprintln!("{err}");
                scripts_succeeded = false;
                break;
            }
        }

        if !scripts_succeeded {
            ExitCode::FAILURE
        } else if !args.scripts.is_empty() && !stdin.is_terminal() {
            ExitCode::SUCCESS
        } else {
            session.run_repl(&mut out)
        }
    };

    if let Some(error) = out.error() {
        eprintln!("write command output: {error}");
        ExitCode::FAILURE
    } else {
        exit
    }
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
    key: Option<String>,
    key_file: Option<PathBuf>,
    copy_text: bool,
}

impl CliArgs {
    /// Resolve the encryption key with `--key` > `--key-file` >
    /// `UQA_KEY` precedence. Only the final trailing newline of a key
    /// file is stripped so keys may contain interior whitespace.
    fn resolve_key(&self) -> Result<Option<String>, String> {
        if let Some(key) = &self.key {
            return Ok(Some(key.clone()));
        }
        if let Some(path) = &self.key_file {
            let raw = std::fs::read_to_string(path)
                .map_err(|err| format!("failed to read key file {}: {err}", path.display()))?;
            let key = raw.strip_suffix('\n').unwrap_or(&raw);
            let key = key.strip_suffix('\r').unwrap_or(key);
            if key.is_empty() {
                return Err(format!("key file {} is empty", path.display()));
            }
            return Ok(Some(key.to_string()));
        }
        match std::env::var("UQA_KEY") {
            Ok(key) if !key.is_empty() => Ok(Some(key)),
            _ => Ok(None),
        }
    }
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
                "--key" => {
                    let Some(key) = args.get(i + 1) else {
                        return Err("--key requires an encryption key".into());
                    };
                    if parsed.key_file.is_some() {
                        return Err("--key and --key-file are mutually exclusive".into());
                    }
                    parsed.key = Some(key.clone());
                    i += 2;
                }
                "--key-file" => {
                    let Some(path) = args.get(i + 1) else {
                        return Err("--key-file requires a path".into());
                    };
                    if parsed.key.is_some() {
                        return Err("--key and --key-file are mutually exclusive".into());
                    }
                    parsed.key_file = Some(PathBuf::from(path));
                    i += 2;
                }
                "--copy-text" => {
                    parsed.copy_text = true;
                    i += 1;
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

fn print_usage_stdout() -> io::Result<()> {
    writeln!(io::stdout().lock(), "{}", usage_text())
}

fn print_usage_stderr() {
    eprintln!("{}", usage_text());
}

fn usage_text() -> &'static str {
    "Usage:\n    usql                        Start with an in-memory database\n    usql --db mydata.db         Start with persistent storage\n    usql script.sql             Execute a SQL script then enter REPL when stdin is a terminal\n    usql --db mydata.db s.sql   Persistent + script\n    usql -c \"SELECT 1\"          Execute a command string and exit\n    usql --copy-text -c \"...\"  Emit rows in PostgreSQL COPY text format\n    usql migrate-python-db <source> <destination>\n\nEncrypted databases:\n    usql --db enc.db --key <key>        Open (or create) an encrypted database\n    usql --db enc.db --key-file <file>  Read the key from a file\n    UQA_KEY=<key> usql --db enc.db      Read the key from the environment\n    Interactive sessions prompt for the key when an encrypted database\n    is opened without one. Compressed containers are detected and\n    opened automatically, including encrypted ones."
}

#[cfg(test)]
#[path = "main_tests.rs"]
mod tests;
