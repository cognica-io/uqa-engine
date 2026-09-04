//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static cast compatibility that depends on declared SQL type identity.

use uqa_sql::ast::ColumnType;
use uqa_sql::SQLError;

use super::common::base_type;

pub(super) fn validate_void_cast(
    source: Option<&ColumnType>,
    target: &ColumnType,
) -> Result<(), SQLError> {
    let target = base_type(target);
    if matches!(target, ColumnType::Void) {
        if source.is_none_or(|source| {
            let source = base_type(source);
            matches!(source, ColumnType::Void) || is_string_io_type(source)
        }) {
            return Ok(());
        }
        return Err(undefined_cast(
            source.expect("known non-string source checked above"),
            target,
        ));
    }
    if source.is_some_and(|source| matches!(base_type(source), ColumnType::Void))
        && !is_string_io_type(target)
    {
        return Err(undefined_cast(
            source.expect("void source checked above"),
            target,
        ));
    }
    Ok(())
}

fn is_string_io_type(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::Text
            | ColumnType::Name
            | ColumnType::Varchar(_)
            | ColumnType::Bpchar
            | ColumnType::Character(_)
    )
}

fn undefined_cast(source: &ColumnType, target: &ColumnType) -> SQLError {
    SQLError::Routine {
        sqlstate: "42846".into(),
        message: format!(
            "cannot cast type {} to {}",
            source.sql_name(),
            target.sql_name()
        ),
    }
}
