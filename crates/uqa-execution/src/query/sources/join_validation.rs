//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static JOIN predicate binding before either input produces rows.

use super::{CteScope, RowSchema, SQLError, SQLParam, ScalarExpr, SourceContext};

pub(super) fn validate_join_on_schema<S: Clone + 'static>(
    context: &SourceContext<'_, S>,
    on: Option<&ScalarExpr>,
    left: &RowSchema,
    right: &RowSchema,
    params: &[SQLParam],
    ctes: &CteScope<S>,
) -> Result<(), SQLError> {
    let Some(on) = on else {
        return Ok(());
    };
    let mut schema = RowSchema::join(left, right, std::iter::empty::<String>());
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
        schema = RowSchema::with_typed_outer_identities(&schema, &identities);
    }
    crate::scalar_type_with_resolver(on, &schema, params, context.types)?;
    Ok(())
}
