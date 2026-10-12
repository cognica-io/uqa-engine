//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admission shared by ROW input analysis and catalog-node construction.

use crate::{ColumnType, SQLError};

pub(crate) fn validate_width(left: usize, right: usize, allow_empty: bool) -> Result<(), SQLError> {
    if left != right {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: "unequal number of entries in row expressions".into(),
        });
    }
    if left == 0 && !allow_empty {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cannot compare rows of zero length".into(),
        });
    }
    Ok(())
}

pub(crate) fn require_boolean(ty: &ColumnType) -> Result<(), SQLError> {
    if *ty == ColumnType::Boolean {
        Ok(())
    } else {
        Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: format!(
                "row comparison operator must yield type boolean, not type {}",
                ty.sql_name()
            ),
        })
    }
}

pub(crate) fn interpretation(
    operator_name: &str,
    interpretations: &[Vec<(i64, i64)>],
) -> Result<i64, SQLError> {
    (1..=6)
        .find(|comparison| {
            interpretations
                .iter()
                .all(|options| options.iter().any(|(candidate, _)| candidate == comparison))
        })
        .ok_or_else(|| SQLError::Diagnostic {
            sqlstate: "0A000".into(),
            message: format!(
                "could not determine interpretation of row comparison operator {operator_name}"
            ),
            detail: None,
            hint: Some(
                "Row comparison operators must be associated with btree operator families.".into(),
            ),
        })
}
