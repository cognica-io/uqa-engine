//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One bounded history policy for the editor, shell commands and persisted entries.

use super::{io, Config, OpenOptions, Path, Session, Write};
use rustyline::history::{DefaultHistory, History};

pub(super) fn load(path: Option<&Path>) -> Result<Vec<String>, String> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let config = Config::builder()
        .max_history_size(1000)
        .map_err(|e| e.to_string())?
        .build();
    let mut history = DefaultHistory::with_config(&config);
    match history.load(path) {
        Ok(()) => Ok(history.iter().cloned().collect()),
        Err(rustyline::error::ReadlineError::Io(error))
            if error.kind() == io::ErrorKind::NotFound =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(format!(
            "failed to read history file {}: {error}",
            path.display()
        )),
    }
}
impl Session {
    pub(super) fn configure_history(&self, editor: &mut super::UsqlEditor) -> Result<(), String> {
        use rustyline::config::Configurer;
        let control = self
            .shell
            .variables
            .get("HISTCONTROL")
            .unwrap_or("ignoreboth");
        editor.set_history_ignore_space(matches!(control, "ignorespace" | "ignoreboth"));
        editor
            .set_history_ignore_dups(matches!(control, "ignoredups" | "ignoreboth"))
            .map_err(|e| e.to_string())?;
        let limit = self
            .shell
            .variables
            .get("HISTSIZE")
            .and_then(|value| value.parse().ok())
            .unwrap_or(1000);
        editor
            .set_max_history_size(limit)
            .map_err(|e| e.to_string())
    }

    pub(super) fn remember_prompt_line(&mut self, line: &str) -> bool {
        let control = self
            .shell
            .variables
            .get("HISTCONTROL")
            .unwrap_or("ignoreboth");
        let limit = self
            .shell
            .variables
            .get("HISTSIZE")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1000);
        if limit == 0 {
            self.history.clear();
            return false;
        }
        let trimmed = line.trim();
        if trimmed.is_empty()
            || (matches!(control, "ignorespace" | "ignoreboth")
                && line.starts_with(char::is_whitespace))
            || (matches!(control, "ignoredups" | "ignoreboth")
                && self.history.last().is_some_and(|entry| entry == trimmed))
        {
            return false;
        }
        self.history.push(trimmed.to_owned());
        let excess = self.history.len().saturating_sub(limit);
        self.history.drain(..excess);
        true
    }
    pub(super) fn record_statement(&mut self, sql: &str) -> Result<(), String> {
        if !self.remember_prompt_line(sql) {
            return Ok(());
        }
        if let Some(path) = &self.history_path {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    format!("create history directory {}: {error}", parent.display())
                })?;
            }
            let mut options = OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options
                .open(path)
                .map_err(|error| format!("open history file {}: {error}", path.display()))?;
            for entry in &self.history {
                writeln!(file, "{entry}")
                    .map_err(|error| format!("write history file {}: {error}", path.display()))?;
            }
        }
        Ok(())
    }
}
