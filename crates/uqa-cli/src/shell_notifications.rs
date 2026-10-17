//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded delivery on the current SQL session. Idle polling exists only after LISTEN.

use super::{io, Engine, ReadlineError, RustylineResult, Session, UsqlEditor, Write};
use rustyline::ExternalPrinter;
use std::time::Duration;

pub(super) fn configure(engine: &Engine) -> Result<(), String> {
    engine
        .configure_sql_notifications(uqa_engine::SQLNotificationOptions {
            max_channels: 256,
            max_queued_notifications: 1024,
            max_queued_bytes: 1024 * 1024,
            max_registry_entries_per_poll: 1024,
        })
        .map_err(|error| error.to_string())
}
fn notification_text(notification: &uqa_core::notifications::SQLNotification) -> String {
    format!("Asynchronous notification \"{}\" with payload \"{}\" received from server process with PID {}.",crate::printing::terminal_text(&notification.channel),crate::printing::terminal_text(&notification.payload),notification.process_id)
}
impl Session {
    pub(super) fn drain_notifications(&self, out: &mut impl Write) -> Result<(), String> {
        if !self.shell.listen_seen {
            return Ok(());
        }
        for notification in self
            .engine
            .try_take_sql_notifications()
            .map_err(|error| crate::diagnostics::sql_error_text(&error))?
        {
            writeln!(out, "{}", notification_text(&notification))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
    pub(super) fn readline(
        &self,
        editor: &mut UsqlEditor,
        prompt: &str,
    ) -> RustylineResult<String> {
        if !self.shell.listen_seen || self.engine.transaction_depth() != 0 {
            return editor.readline(prompt);
        }
        let mut printer = editor.create_external_printer()?;
        let (stop, receive) = std::sync::mpsc::channel();
        let engine = &self.engine;
        std::thread::scope(|scope| {
            let worker = scope.spawn(move || -> Result<(), String> {
                while receive.recv_timeout(Duration::from_millis(250))
                    == Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                {
                    let notifications = engine
                        .try_take_sql_notifications()
                        .map_err(|error| crate::diagnostics::sql_error_text(&error))?;
                    for notification in notifications {
                        printer
                            .print(notification_text(&notification))
                            .map_err(|error| error.to_string())?;
                    }
                }
                Ok(())
            });
            let result = editor.readline(prompt);
            let _ = stop.send(());
            worker
                .join()
                .map_err(|_| ReadlineError::Io(io::Error::other("notification reader panicked")))?
                .map_err(|error| ReadlineError::Io(io::Error::other(error)))?;
            result
        })
    }
}
