//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Client-side input state. SQL compilation and transaction execution remain Engine-owned.

use super::{
    io, open_engine, print_result_copy_text_with_engine, statement_is_pure_comment, BufRead,
    IsTerminal, Path, PathBuf, PromptLineOutcome, SQLResult, Session, Value, Write,
};
use std::io::Read;

use crate::lexer::{tokenize_sql, TokenKind};
use crate::printing::{DisplayTable, PrintOptions};
use crate::variables::{parse_boolean, Variables};

#[derive(Default)]
pub(super) enum ResultTarget {
    #[default]
    Print,
    Capture(Option<Box<SQLResult>>),
}

#[derive(Default)]
pub(super) struct ShellState {
    pub variables: Variables,
    pub printing: PrintOptions,
    pub last_query: String,
    pub includes: Vec<PathBuf>,
    pub quit: bool,
    pub listen_seen: bool,
    pub result_target: ResultTarget,
    pub clear_history: bool,
}

impl Session {
    pub(super) fn process_line(
        &mut self,
        line: &str,
        buffer: &mut String,
        out: &mut impl Write,
        history: bool,
    ) -> PromptLineOutcome {
        let _input_scope = crate::interrupt::InputScope::enter();
        match self.process_line_result(line, buffer, out, history) {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = writeln!(out, "ERROR: {error}");
                PromptLineOutcome::Failed
            }
        }
    }

    fn process_line_result(
        &mut self,
        line: &str,
        buffer: &mut String,
        out: &mut impl Write,
        history: bool,
    ) -> Result<PromptLineOutcome, String> {
        if buffer.len().saturating_add(line.len()) > 16 * 1024 * 1024 {
            return Err("query buffer exceeds 16 MiB".into());
        }
        let source = format!("{buffer}\n{line}");
        let base = buffer.len() + 1;
        let meta = tokenize_sql(&source).into_iter().find(|token| {
            token.span.start >= base && token.kind == TokenKind::Meta && token.text(&source) != "\\"
        });
        let sql = meta
            .as_ref()
            .map_or(line, |token| &line[..token.span.start - base]);
        if buffer.len().saturating_add(sql.len()) > 16 * 1024 * 1024 {
            return Err("query buffer exceeds 16 MiB".into());
        }
        if !sql.is_empty() {
            if !buffer.is_empty() {
                buffer.push('\n');
            }
            buffer.push_str(sql);
        }
        self.dispatch_complete_input(buffer, out, history)?;
        if let Some(token) = meta {
            let command = &line[token.span.start - base + 1..];
            let (name, argument) = command
                .split_once(char::is_whitespace)
                .unwrap_or((command, ""));
            if self.shell.variables.conditional(name, argument.trim())? {
                // Each branch starts a fresh query buffer. Inactive text must never leak into an active branch.
                buffer.clear();
                return Ok(PromptLineOutcome::Continue);
            }
            if !self.shell.variables.active() {
                return Ok(PromptLineOutcome::Continue);
            }
            if let Some(outcome) = self.handle_shell_meta(name, argument.trim(), buffer, out)? {
                return Ok(outcome);
            }
            return Ok(self.handle_meta(command, out));
        }
        Ok(PromptLineOutcome::Continue)
    }

    fn dispatch_complete_input(
        &mut self,
        buffer: &mut String,
        out: &mut impl Write,
        history: bool,
    ) -> Result<(), String> {
        loop {
            let Some(offset) = crate::statements::input_terminator_offsets(buffer)
                .first()
                .copied()
            else {
                break;
            };
            let remainder = buffer.split_off(offset + 1);
            let query = std::mem::replace(buffer, remainder);
            if self.shell.variables.active() {
                let query = self.shell.variables.expand(&query);
                self.shell.last_query.clone_from(&query);
                self.execute_text_with_history(&query, out, history)?;
            }
        }
        if statement_is_pure_comment(buffer) {
            buffer.clear();
        }
        Ok(())
    }

    pub(super) fn finish_input(
        &mut self,
        buffer: &mut String,
        out: &mut impl Write,
    ) -> Result<(), String> {
        if self.shell.variables.depth() != 0 {
            return Err("reached end of input without finding matching \\endif".into());
        }
        if !buffer.trim().is_empty() && !statement_is_pure_comment(buffer) {
            let query = self.shell.variables.expand(&std::mem::take(buffer));
            self.shell.last_query.clone_from(&query);
            self.execute_text(&query, out)?;
        }
        Ok(())
    }

    pub(super) fn include_script(
        &mut self,
        path: &Path,
        out: &mut impl Write,
    ) -> Result<(), String> {
        let path = path
            .canonicalize()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if self.shell.includes.len() >= 32 || self.shell.includes.contains(&path) {
            return Err("recursive or too deeply nested script inclusion".into());
        }
        let file =
            std::fs::File::open(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let depth = self.shell.variables.begin_script();
        self.shell.includes.push(path.clone());
        let result = self.read_script(io::BufReader::new(file), out, depth);
        self.shell.includes.pop();
        self.shell.variables.end_script();
        result.map_err(|error| format!("{}: {error}", path.display()))
    }

    fn read_script(
        &mut self,
        mut reader: impl BufRead,
        out: &mut impl Write,
        depth: usize,
    ) -> Result<(), String> {
        let mut buffer = String::new();
        let mut line = String::new();
        let mut failure = None;
        let mut number = 0;
        let input_scope = crate::interrupt::InputScope::enter();
        let generation = input_scope.generation;
        loop {
            if crate::interrupt::generation() != generation {
                return Err("canceling script due to user request".into());
            }
            line.clear();
            let bytes = reader
                .by_ref()
                .take(16 * 1024 * 1024 + 1)
                .read_line(&mut line)
                .map_err(|error| error.to_string())?;
            if crate::interrupt::generation() != generation {
                return Err("canceling script due to user request".into());
            }
            if bytes == 0 {
                break;
            }
            number += 1;
            let outcome = self.process_line_result(
                line.trim_end_matches(['\n', '\r']),
                &mut buffer,
                out,
                false,
            );
            if crate::interrupt::generation() != generation {
                return Err("canceling script due to user request".into());
            }
            match outcome {
                Ok(PromptLineOutcome::Quit) => {
                    self.shell.quit = true;
                    return Ok(());
                }
                Ok(PromptLineOutcome::Failed) => {
                    let error = format!("line {number}: meta command failed");
                    if self.shell.variables.flag("ON_ERROR_STOP", false) {
                        return Err(error);
                    }
                    failure = Some(error);
                }
                Ok(PromptLineOutcome::Continue) => {}
                Err(error) => {
                    let error = format!("line {number}: {error}");
                    if self.shell.variables.flag("ON_ERROR_STOP", false) {
                        return Err(error);
                    }
                    writeln!(out, "ERROR: {error}").map_err(|e| e.to_string())?;
                    failure = Some(error);
                }
            }
        }
        if self.shell.variables.depth() != depth {
            return Err("unmatched conditional in included script".into());
        }
        if !buffer.trim().is_empty() && self.shell.variables.active() {
            let query = self.shell.variables.expand(&buffer);
            self.shell.last_query.clone_from(&query);
            self.execute_text_with_history(&query, out, false)?;
        }
        failure.map_or(Ok(()), Err)
    }

    fn handle_shell_meta(
        &mut self,
        name: &str,
        argument: &str,
        buffer: &mut String,
        out: &mut impl Write,
    ) -> Result<Option<PromptLineOutcome>, String> {
        if self.catalog_command(name, argument, out)? {
            return Ok(Some(PromptLineOutcome::Continue));
        }
        // COPY uses SQL quoting and deliberately does not substitute shell variables.
        if name == "copy" {
            self.copy_file(argument, out)?;
            return Ok(Some(PromptLineOutcome::Continue));
        }
        let args = crate::arguments::parse(argument, &self.shell.variables)?;
        if self.variable_command(name, &args, out)? {
            return Ok(Some(PromptLineOutcome::Continue));
        }
        match name {
            "p" => {
                no_args(&args)?;
                writeln!(
                    out,
                    "{}",
                    crate::printing::message_text(if buffer.trim().is_empty() {
                        &self.shell.last_query
                    } else {
                        buffer
                    })
                )
                .map_err(|e| e.to_string())?;
            }
            "r" => {
                no_args(&args)?;
                buffer.clear();
                writeln!(out, "Query buffer reset (cleared).").map_err(|e| e.to_string())?;
            }
            "g" | "gx" | "gset" => self.submit_buffer(name, &args, buffer, out)?,
            "o" => {
                if args.len() > 1 {
                    return Err("usage: \\o [file]".into());
                }
                self.handle_output_redirect(args.first().map_or("", String::as_str), out)?;
            }
            "timing" => {
                self.show_timing = optional_boolean(&args, self.show_timing)?;
                writeln!(
                    out,
                    "Timing is {}.",
                    if self.show_timing { "on" } else { "off" }
                )
                .map_err(|e| e.to_string())?;
            }
            "pset" | "x" | "expanded" | "a" | "H" | "t" | "f" | "C" => {
                self.set_printing(name, &args, out)?;
            }
            "i" | "include" | "ir" | "include_relative" | "run" => {
                let mut path = PathBuf::from(single_arg(&args)?);
                if matches!(name, "ir" | "include_relative") && path.is_relative() {
                    if let Some(parent) = self.shell.includes.last().and_then(|p| p.parent()) {
                        path = parent.join(path);
                    }
                }
                self.include_script(&path, out)?;
            }
            "w" | "s" | "e" | "edit" => self.file_command(name, &args, buffer, out)?,
            "watch" => self.watch_query(&args, buffer, out)?,
            "c" | "connect" | "open" => {
                if name == "open" && args.len() != 1 {
                    return Err("usage: \\open <path>".into());
                }
                if args.len() > 1 {
                    return Err("usage: \\connect [database path]".into());
                }
                let memory = args.first().is_some_and(|path| path == ":memory:");
                let path = args
                    .first()
                    .filter(|path| path.as_str() != "-")
                    .map(PathBuf::from)
                    .or_else(|| self.db_path.clone());
                let path = if memory { None } else { path };
                let key = if path == self.db_path {
                    self.db_key.as_deref()
                } else {
                    None
                };
                let (engine, location, key) = open_engine(path.as_deref(), key)?;
                crate::shell_notifications::configure(&engine)?;
                self.engine = engine;
                self.db_path = path;
                self.db_key = key;
                self.location = location;
                self.shell.listen_seen = false;
                self.connection_info(out)?;
            }
            "conninfo" => {
                no_args(&args)?;
                self.connection_info(out)?;
            }
            _ => return Ok(None),
        }
        Ok(Some(if self.shell.quit {
            PromptLineOutcome::Quit
        } else {
            PromptLineOutcome::Continue
        }))
    }

    fn variable_command(
        &mut self,
        name: &str,
        args: &[String],
        out: &mut impl Write,
    ) -> Result<bool, String> {
        match name {
            "set" => {
                if let Some(key) = args.first() {
                    let value = if args.len() == 1
                        && matches!(key.as_str(), "ON_ERROR_STOP" | "AUTOCOMMIT" | "HIGHLIGHT")
                    {
                        "on".into()
                    } else {
                        args[1..].concat()
                    };
                    self.shell.variables.set(key, value)?;
                } else {
                    for (key, value) in self.shell.variables.entries() {
                        writeln!(out, "{key} = '{}'", crate::printing::terminal_text(value))
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            "unset" => self.shell.variables.unset(single_arg(args)?)?,
            "echo" | "qecho" => {
                let newline = args.first().is_none_or(|arg| arg != "-n");
                let values = if newline { args } else { &args[1..] };
                let text = values.join(" ") + if newline { "\n" } else { "" };
                let text = if io::stdout().is_terminal()
                    && (name != "qecho" || self.output_path.is_none())
                {
                    crate::printing::message_text(&text)
                } else {
                    text
                };
                if name == "qecho" {
                    self.write_query_output(out, |writer| {
                        let _ = write!(writer, "{text}");
                    })?;
                } else {
                    write!(out, "{text}").map_err(|e| e.to_string())?;
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn connection_info(&self, out: &mut impl Write) -> Result<(), String> {
        writeln!(
            out,
            "You are connected to database \"{}\" through the embedded Engine.",
            crate::printing::terminal_text(&self.location)
        )
        .map_err(|e| e.to_string())
    }

    fn submit_buffer(
        &mut self,
        command: &str,
        args: &[String],
        buffer: &mut String,
        out: &mut impl Write,
    ) -> Result<(), String> {
        if args.len() > 1 {
            return Err(format!("usage: \\{command} [argument]"));
        }
        let query = if buffer.trim().is_empty() {
            self.shell.last_query.clone()
        } else {
            std::mem::take(buffer)
        };
        if query.trim().is_empty() {
            return Ok(());
        }
        let query = self.shell.variables.expand(&query);
        self.shell.last_query.clone_from(&query);
        if command == "gset" {
            let result = self.capture_query(&query, out)?;
            if result.rows.len() != 1 {
                return Err(format!(
                    "\\gset: expected one row, got {}",
                    result.rows.len()
                ));
            }
            let mut updates = Vec::new();
            for (column, name) in result.columns.iter().enumerate() {
                let key = format!("{}{name}", args.first().map_or("", String::as_str));
                let value = match result.value_at(0, column) {
                    None | Some(Value::Null) => None,
                    value => Some(
                        crate::output::value_to_display_typed(
                            value,
                            result.column_types.get(column).and_then(Option::as_ref),
                            Some(&self.engine),
                        )
                        .map_err(|e| crate::diagnostics::sql_error_text(&e))?,
                    ),
                };
                let mut validation = Variables::default();
                if let Some(value) = &value {
                    validation.set(&key, value.clone())?;
                } else {
                    validation.unset(&key)?;
                }
                updates.push((key, value));
            }
            for (key, value) in updates {
                if let Some(value) = value {
                    self.shell.variables.set(&key, value)?;
                } else {
                    self.shell.variables.unset(&key)?;
                }
            }
            return Ok(());
        }
        let old_output = self.output_path.clone();
        let old_expanded = self.expanded;
        if let Some(path) = args.first() {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(path)
                .map_err(|error| format!("{path}: {error}"))?;
            self.output_path = Some(PathBuf::from(path));
        }
        if command == "gx" {
            self.expanded = true;
        }
        let result = self.execute_command_text_with_history(&query, out, true);
        self.output_path = old_output;
        self.expanded = old_expanded;
        result
    }

    pub(super) fn capture_query(
        &mut self,
        query: &str,
        out: &mut impl Write,
    ) -> Result<SQLResult, String> {
        self.shell.result_target = ResultTarget::Capture(None);
        let outcome = self.execute_command_text_with_history(query, out, true);
        let result = match std::mem::take(&mut self.shell.result_target) {
            ResultTarget::Capture(result) => result.map(|result| *result),
            ResultTarget::Print => None,
        };
        outcome?;
        result.ok_or_else(|| "query returned no result".into())
    }

    pub(super) fn prepare_statement_transaction(&self, sql: &str) -> Result<(), String> {
        if !self.shell.variables.flag("AUTOCOMMIT", true)
            && self.engine.transaction_depth() == 0
            && !transaction_control(sql)
        {
            self.engine
                .sql("BEGIN", &[])
                .map_err(|e| crate::diagnostics::sql_error_text(&e))?;
        }
        Ok(())
    }

    pub(super) fn query(&mut self, sql: &str) -> Result<SQLResult, String> {
        let _interrupt = crate::interrupt::QueryInterrupt::new(self.engine.cancellation_token())?;
        self.prepare_statement_transaction(sql)?;
        let updates_listen = tokenize_sql(sql).iter().any(|token| {
            token.kind == TokenKind::Word
                && matches!(
                    token.text(sql).to_ascii_uppercase().as_str(),
                    "LISTEN" | "UNLISTEN" | "COMMIT" | "END" | "ROLLBACK" | "ABORT"
                )
        });
        let result = {
            self.engine
                .sql(sql, &[])
                .map_err(|e| crate::diagnostics::sql_error_text(&e))
        };
        if result.is_ok() && updates_listen {
            self.shell.listen_seen = !self
                .engine
                .sql("SELECT * FROM pg_listening_channels()", &[])
                .map_err(|e| crate::diagnostics::sql_error_text(&e))?
                .rows
                .is_empty();
        }
        result
    }

    pub(super) fn render_result(
        &mut self,
        result: &SQLResult,
        out: &mut impl Write,
    ) -> Result<(), String> {
        if self.copy_text {
            let mut error = Ok(());
            self.write_query_output(out, |writer| {
                error = print_result_copy_text_with_engine(result, &self.engine, writer);
            })?;
            return error.map_err(|e| crate::diagnostics::sql_error_text(&e));
        }
        let table = DisplayTable::from_result(result, Some(&self.engine))
            .map_err(|e| crate::diagnostics::sql_error_text(&e))?;
        let terminal = self.output_path.is_none() && io::stdout().is_terminal();
        let rendered = if terminal {
            self.shell.printing.render_terminal(&table, self.expanded)
        } else {
            self.shell.printing.render(&table, self.expanded)
        };
        self.print_rendered(&rendered, out)
    }
}

fn transaction_control(sql: &str) -> bool {
    let tokens = tokenize_sql(sql);
    let mut words = tokens
        .iter()
        .filter(|token| !matches!(token.kind, TokenKind::Space | TokenKind::Comment))
        .map(|token| token.text(sql).to_ascii_uppercase());
    let first = words.next().unwrap_or_default();
    matches!(
        first.as_str(),
        "BEGIN" | "START" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" | "VACUUM"
    ) || (first == "DISCARD" && words.next().is_some_and(|word| word == "ALL"))
}

pub(super) fn single_arg(args: &[String]) -> Result<&str, String> {
    if let [value] = args {
        Ok(value)
    } else {
        Err("expected one argument".into())
    }
}
pub(super) fn no_args(args: &[String]) -> Result<(), String> {
    if args.is_empty() {
        Ok(())
    } else {
        Err("unexpected command argument".into())
    }
}
pub(super) fn optional_boolean(args: &[String], current: bool) -> Result<bool, String> {
    if args.is_empty() {
        Ok(!current)
    } else {
        parse_boolean(single_arg(args)?)
    }
}
