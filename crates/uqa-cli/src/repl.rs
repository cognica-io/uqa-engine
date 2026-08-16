//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session state, interactive and line REPL loops, and SQL execution.

use super::*;

impl Session {
    pub(super) fn run_repl(&mut self, out: &mut impl Write) -> ExitCode {
        if let Err(error) = self.print_banner(out) {
            eprintln!("write banner: {error}");
            return ExitCode::FAILURE;
        }
        if io::stdin().is_terminal() {
            return self.run_interactive_repl(out);
        }
        self.run_line_repl(out)
    }

    pub(super) fn run_interactive_repl(&mut self, out: &mut impl Write) -> ExitCode {
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
        if let Err(error) = self.load_readline_history(&mut editor) {
            let _ = writeln!(out, "history load error: {error}");
            return ExitCode::FAILURE;
        }
        let mut buffer = String::new();
        loop {
            let helper = match self.repl_helper() {
                Ok(helper) => helper,
                Err(err) => {
                    let _ = writeln!(out, "completion catalog error: {err}");
                    return ExitCode::FAILURE;
                }
            };
            editor.set_helper(Some(helper));
            let prompt = if buffer.is_empty() {
                PROMPT_PRIMARY
            } else {
                PROMPT_CONTINUATION
            };
            match editor.readline(prompt) {
                Ok(line) => {
                    self.remember_prompt_line(&line);
                    if matches!(
                        self.handle_prompt_line_with_history(&line, &mut buffer, out, false),
                        PromptLineOutcome::Quit
                    ) {
                        return match self.append_readline_history(&mut editor) {
                            Ok(()) => ExitCode::SUCCESS,
                            Err(error) => {
                                let _ = writeln!(out, "history save error: {error}");
                                ExitCode::FAILURE
                            }
                        };
                    }
                }
                Err(ReadlineError::Interrupted) => {
                    buffer.clear();
                    let _ = writeln!(out);
                }
                Err(ReadlineError::Eof) => {
                    let _ = writeln!(out);
                    return match self.append_readline_history(&mut editor) {
                        Ok(()) => ExitCode::SUCCESS,
                        Err(error) => {
                            let _ = writeln!(out, "history save error: {error}");
                            ExitCode::FAILURE
                        }
                    };
                }
                Err(err) => {
                    let _ = writeln!(out, "readline error: {err}");
                    if let Err(error) = self.append_readline_history(&mut editor) {
                        let _ = writeln!(out, "history save error: {error}");
                    }
                    return ExitCode::FAILURE;
                }
            }
        }
    }

    pub(super) fn run_line_repl(&mut self, out: &mut impl Write) -> ExitCode {
        let stdin = io::stdin();
        let mut input = String::new();
        let mut buffer = String::new();
        let mut had_error = false;
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
                    if !buffer.trim().is_empty() {
                        let _ = writeln!(out, "ERROR: unterminated SQL statement at end of input");
                        return ExitCode::FAILURE;
                    }
                    let _ = writeln!(out);
                    return if had_error {
                        ExitCode::FAILURE
                    } else {
                        ExitCode::SUCCESS
                    };
                }
                Ok(_) => {}
                Err(e) => {
                    let _ = writeln!(out, "stdin error: {e}");
                    return ExitCode::FAILURE;
                }
            }
            let line = input.trim_end_matches(['\n', '\r']);
            match self.handle_prompt_line(line, &mut buffer, out) {
                PromptLineOutcome::Continue => {}
                PromptLineOutcome::Failed => had_error = true,
                PromptLineOutcome::Quit => {
                    return if had_error {
                        ExitCode::FAILURE
                    } else {
                        ExitCode::SUCCESS
                    };
                }
            }
        }
    }

    pub(super) fn handle_prompt_line(
        &mut self,
        line: &str,
        buffer: &mut String,
        out: &mut impl Write,
    ) -> PromptLineOutcome {
        self.handle_prompt_line_with_history(line, buffer, out, true)
    }

    pub(super) fn handle_prompt_line_with_history(
        &mut self,
        line: &str,
        buffer: &mut String,
        out: &mut impl Write,
        record_sql_history: bool,
    ) -> PromptLineOutcome {
        if buffer.is_empty() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return PromptLineOutcome::Continue;
            }
            if let Some(rest) = trimmed.strip_prefix('\\') {
                return self.handle_meta(rest, out);
            }
        }
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(line);
        if contains_statement_terminator(buffer) {
            if let Err(err) = self.execute_text_with_history(buffer, out, record_sql_history) {
                let _ = writeln!(out, "ERROR: {err}");
                buffer.clear();
                return PromptLineOutcome::Failed;
            }
            buffer.clear();
        }
        PromptLineOutcome::Continue
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PromptLineOutcome {
    Continue,
    Failed,
    Quit,
}

pub(super) struct Session {
    pub(super) engine: Engine,
    pub(super) db_path: Option<PathBuf>,
    /// Encryption key of the currently open database, kept so `\reset`
    /// and re-opens go through the same keyed path.
    pub(super) db_key: Option<String>,
    pub(super) location: String,
    pub(super) history: Vec<String>,
    pub(super) history_path: Option<PathBuf>,
    pub(super) show_timing: bool,
    pub(super) expanded: bool,
    pub(super) copy_text: bool,
    pub(super) output_path: Option<PathBuf>,
}

impl Session {
    pub(super) fn new(db_path: Option<PathBuf>, key: Option<&str>) -> Result<Self, String> {
        Self::new_with_history_path(db_path, key, history_path())
    }

    pub(super) fn new_without_history(
        db_path: Option<PathBuf>,
        key: Option<&str>,
    ) -> Result<Self, String> {
        Self::new_with_history_path(db_path, key, None)
    }

    fn new_with_history_path(
        db_path: Option<PathBuf>,
        key: Option<&str>,
        history_path: Option<PathBuf>,
    ) -> Result<Self, String> {
        let history = match history_path.as_ref() {
            Some(path) => match std::fs::read_to_string(path) {
                Ok(text) => text
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(std::string::ToString::to_string)
                    .collect(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
                Err(error) => {
                    return Err(format!(
                        "failed to read history file {}: {error}",
                        path.display()
                    ));
                }
            },
            None => Vec::new(),
        };
        let (engine, location, db_key) = open_engine(db_path.as_deref(), key)?;
        Ok(Self {
            engine,
            db_path,
            db_key,
            location,
            history,
            history_path,
            show_timing: false,
            expanded: false,
            copy_text: false,
            output_path: None,
        })
    }

    pub(super) fn repl_helper(&self) -> Result<UsqlHelper, String> {
        let mut columns = BTreeSet::new();
        let table_names = self.engine.table_names().map_err(|err| err.to_string())?;
        for table in &table_names {
            if let Some(defs) = self
                .engine
                .describe_table(table)
                .map_err(|err| err.to_string())?
            {
                columns.extend(defs.into_iter().map(|def| def.name));
            }
        }
        let foreign_tables = self.engine.list_foreign_tables()?;
        for table in &foreign_tables {
            columns.extend(self.engine.foreign_table_columns(table)?);
        }
        Ok(UsqlHelper::new(
            table_names,
            foreign_tables,
            columns.into_iter().collect(),
        ))
    }

    pub(super) fn load_readline_history(&self, editor: &mut UsqlEditor) -> Result<(), String> {
        let Some(path) = &self.history_path else {
            return Ok(());
        };
        match editor.load_history(path) {
            Ok(()) => Ok(()),
            Err(ReadlineError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("{}: {error}", path.display())),
        }
    }

    pub(super) fn append_readline_history(&self, editor: &mut UsqlEditor) -> Result<(), String> {
        let Some(path) = &self.history_path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                format!("create history directory {}: {error}", parent.display())
            })?;
        }
        editor
            .append_history(path)
            .map_err(|error| format!("append {}: {error}", path.display()))
    }

    pub(super) fn remember_prompt_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.last().is_some_and(|entry| entry == trimmed) {
            return;
        }
        self.history.push(trimmed.to_string());
    }

    pub(super) fn record_statement(&mut self, sql: &str) -> Result<(), String> {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        if self.history.last().is_some_and(|l| l == trimmed) {
            return Ok(());
        }
        if let Some(path) = &self.history_path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!("create history directory {}: {error}", parent.display())
                })?;
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| format!("open history file {}: {error}", path.display()))?;
            writeln!(file, "{trimmed}")
                .map_err(|error| format!("write history file {}: {error}", path.display()))?;
        }
        self.history.push(trimmed.to_string());
        Ok(())
    }

    pub(super) fn print_banner(&self, out: &mut impl Write) -> io::Result<()> {
        writeln!(
            out,
            "usql {} -- UQA interactive SQL shell",
            env!("CARGO_PKG_VERSION")
        )?;
        writeln!(out, "Database: {}", self.location)?;
        writeln!(out, "Type SQL statements terminated by ';'")?;
        writeln!(out, "Use \\? for help, \\q to quit.")?;
        writeln!(out)
    }

    pub(super) fn run_file(&mut self, path: &Path, out: &mut impl Write) -> Result<(), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| format!("File not found or unreadable: {}: {err}", path.display()))?;
        self.execute_text(&text, out)
    }

    pub(super) fn execute_text(&mut self, text: &str, out: &mut impl Write) -> Result<(), String> {
        self.execute_text_with_history(text, out, true)
    }

    pub(super) fn execute_text_with_history(
        &mut self,
        text: &str,
        out: &mut impl Write,
        record_history: bool,
    ) -> Result<(), String> {
        for stmt in split_statements(text) {
            if statement_is_pure_comment(&stmt) {
                continue;
            }
            self.run_statement_with_history(&stmt, out, record_history)?;
        }
        Ok(())
    }

    pub(super) fn run_statement_with_history(
        &mut self,
        sql: &str,
        out: &mut impl Write,
        record_history: bool,
    ) -> Result<(), String> {
        if record_history {
            self.record_statement(sql)?;
        }
        let start = std::time::Instant::now();
        let outcome = self.engine.sql(sql, &[]);
        let elapsed = start.elapsed();
        let result = match outcome {
            Ok(result) => self.write_query_output(out, |writer| {
                if self.copy_text {
                    print_result_copy_text(&result, writer);
                } else if self.expanded {
                    print_result_expanded(&result, writer);
                } else {
                    print_result(&result, writer);
                }
            }),
            Err(err) => Err(format!("{}: {err}", err.sqlstate().unwrap_or("XX000"))),
        };
        let timing_result = if self.show_timing {
            let ms = elapsed.as_secs_f64() * 1000.0;
            self.write_query_output(out, |writer| {
                let _ = writeln!(writer, "Time: {ms:.3} ms");
            })
        } else {
            Ok(())
        };
        result.and(timing_result)
    }

    pub(super) fn write_query_output(
        &self,
        out: &mut impl Write,
        write: impl FnOnce(&mut dyn Write),
    ) -> Result<(), String> {
        if let Some(path) = &self.output_path {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map_err(|error| format!("output failed: {}: {error}", path.display()))?;
            let mut tracked = TrackedWriter::new(&mut file);
            write(&mut tracked);
            let flush_result = tracked.flush();
            if let Some(error) = tracked.error() {
                return Err(format!("output failed: {}: {error}", path.display()));
            }
            flush_result.map_err(|error| format!("output failed: {}: {error}", path.display()))
        } else {
            let mut tracked = TrackedWriter::new(out);
            write(&mut tracked);
            let flush_result = tracked.flush();
            if let Some(error) = tracked.error() {
                return Err(format!("write query output: {error}"));
            }
            flush_result.map_err(|error| format!("flush query output: {error}"))
        }
    }
}
