//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered RAISE diagnostic options, independent of expression execution.

use super::{condition_sqlstate, runtime_diagnostics::looks_like_sqlstate, PLpgSQLExpression};
use crate::SQLError;

#[derive(Debug, Clone, Copy)]
pub enum RaiseOptionKind {
    ErrorCode,
    Message,
    Detail,
    Hint,
}

#[derive(Debug, Clone)]
pub struct RaiseOption {
    pub kind: RaiseOptionKind,
    pub value: PLpgSQLExpression,
}

#[derive(Default)]
pub struct RaiseDiagnostic {
    pub sqlstate: Option<String>,
    pub condition: Option<String>,
    pub message: Option<String>,
    pub detail: Option<String>,
    pub hint: Option<String>,
}

impl RaiseDiagnostic {
    /// The executor evaluates and converts each value before checking whether that field was already supplied.
    pub fn option(&mut self, kind: RaiseOptionKind, value: String) -> Result<(), SQLError> {
        let (slot, name) = match kind {
            RaiseOptionKind::ErrorCode => {
                if self
                    .sqlstate
                    .as_deref()
                    .is_some_and(|state| state != "00000")
                {
                    return Err(duplicate("ERRCODE"));
                }
                self.sqlstate = Some(
                    if looks_like_sqlstate(&value) && !value.bytes().any(|b| b.is_ascii_lowercase())
                    {
                        value.clone()
                    } else {
                        condition_sqlstate(&value)
                            .ok_or_else(|| SQLError::Routine {
                                sqlstate: "42704".into(),
                                message: format!("unrecognized exception condition \"{value}\""),
                            })?
                            .to_owned()
                    },
                );
                self.condition = Some(value);
                return Ok(());
            }
            RaiseOptionKind::Message => (&mut self.message, "MESSAGE"),
            RaiseOptionKind::Detail => (&mut self.detail, "DETAIL"),
            RaiseOptionKind::Hint => (&mut self.hint, "HINT"),
        };
        if slot.is_some() {
            return Err(duplicate(name));
        }
        *slot = Some(value);
        Ok(())
    }
}

fn duplicate(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: format!("RAISE option already specified: {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_sqlstate_can_be_replaced_but_nonzero_state_cannot() {
        let mut diagnostic = RaiseDiagnostic::default();
        diagnostic
            .option(RaiseOptionKind::ErrorCode, "00000".into())
            .unwrap();
        diagnostic
            .option(RaiseOptionKind::ErrorCode, "division_by_zero".into())
            .unwrap();
        assert_eq!(diagnostic.sqlstate.as_deref(), Some("22012"));
        assert_eq!(
            diagnostic
                .option(RaiseOptionKind::ErrorCode, "22023".into())
                .unwrap_err()
                .sqlstate(),
            Some("42601")
        );
    }

    #[test]
    fn dynamic_sqlstate_is_not_case_folded_and_empty_text_is_still_supplied() {
        let mut diagnostic = RaiseDiagnostic::default();
        assert_eq!(
            diagnostic
                .option(RaiseOptionKind::ErrorCode, "22p02".into())
                .unwrap_err()
                .sqlstate(),
            Some("42704")
        );
        diagnostic
            .option(RaiseOptionKind::Message, String::new())
            .unwrap();
        assert_eq!(
            diagnostic
                .option(RaiseOptionKind::Message, "another".into())
                .unwrap_err()
                .sqlstate(),
            Some("42601")
        );
    }
}
