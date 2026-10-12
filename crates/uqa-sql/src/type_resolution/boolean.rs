//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Boolean contexts validate base types while retaining declared diagnostic names.

use crate::{ColumnType, FunctionTypeResolver, SQLError};

pub(crate) fn require_argument_type(
    ty: Option<&ColumnType>,
    context: &str,
    resolver: Option<&dyn FunctionTypeResolver>,
) -> Result<(), SQLError> {
    let Some(ty) = ty else { return Ok(()) };
    if matches!(super::base_type(ty), ColumnType::Boolean) {
        Ok(())
    } else {
        let name = resolver.map_or_else(|| Ok(ty.regtype_name()), |r| r.format_type_name(ty))?;
        Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: format!("argument of {context} must be type boolean, not type {name}"),
        })
    }
}
