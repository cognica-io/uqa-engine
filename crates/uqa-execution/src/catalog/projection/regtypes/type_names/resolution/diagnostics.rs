//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected type diagnostics stream borrowed names into one admitted destination.

use uqa_core::memory::ProductionControl;
use uqa_sql::SQLError;

#[cfg(test)]
mod tests;

pub(super) struct QualifiedNames<'a>(pub(super) &'a [String]);

impl std::fmt::Display for QualifiedNames<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, name) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(".")?;
            }
            formatter.write_str(name)?;
        }
        Ok(())
    }
}

pub(super) fn routine(
    state: &str,
    arguments: std::fmt::Arguments<'_>,
    control: &ProductionControl<'_>,
) -> SQLError {
    match control.format(arguments) {
        Ok(message) => {
            let (message, _memory) = message.into_parts();
            SQLError::Routine {
                sqlstate: state.into(),
                message,
            }
        }
        Err(error) => error.into(),
    }
}

pub(super) fn cross_database(names: &[String], control: &ProductionControl<'_>) -> SQLError {
    match control.format(format_args!(
        "cross-database references are not implemented: {}",
        QualifiedNames(names),
    )) {
        Ok(message) => {
            let (message, _memory) = message.into_parts();
            SQLError::Unsupported(message)
        }
        Err(error) => error.into(),
    }
}
