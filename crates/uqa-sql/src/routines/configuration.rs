//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL rules for configuration saved on a routine definition.

use crate::{ast::RoutineConfigAction, SQLError};

pub fn validate_routine_config_action(action: &RoutineConfigAction) -> Result<(), SQLError> {
    match action {
        RoutineConfigAction::Set { name, .. } | RoutineConfigAction::FromCurrent { name }
            if name.eq_ignore_ascii_case("transaction_isolation")
                || name.eq_ignore_ascii_case("transaction_read_only")
                || name.eq_ignore_ascii_case("transaction_deferrable") =>
        {
            Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: format!("parameter \"{name}\" cannot be set locally in functions"),
            })
        }
        _ => Ok(()),
    }
}
