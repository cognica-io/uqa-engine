//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static JOIN predicate binding before either input produces rows.

use super::{CteScope, Engine, SQLError, SQLParam, ScalarExpr};

pub(super) fn validate_join_on_schema(
    engine: &Engine,
    on: Option<&ScalarExpr>,
    left: &uqa_execution::RowSchema,
    right: &uqa_execution::RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let Some(on) = on else {
        return Ok(());
    };
    let mut schema = uqa_execution::RowSchema::join(left, right, std::iter::empty::<String>());
    if let Some(outer) = ctes.row_lock_outer_row() {
        let identities = outer
            .schema
            .identities()
            .iter()
            .enumerate()
            .map(|(position, identity)| {
                (
                    identity.clone(),
                    outer.schema.column_type(position).cloned(),
                )
            })
            .collect::<Vec<_>>();
        schema = uqa_execution::RowSchema::with_typed_outer_identities(&schema, &identities);
    }
    uqa_execution::scalar_type_with_resolver(on, &schema, params, engine)?;
    Ok(())
}
