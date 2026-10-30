//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Capture selected view metadata from the borrowed native definition before releasing its generation.

use super::{Produced, ProductionControl, SQLError, ViewMutationTarget, ViewRewriteContext};
use crate::{
    catalog::{stored_view::StoredView, view::StoredViewKind},
    plan::copy::SyntaxProduction,
    semantics::view_rewrite::context::stored_view_schema_with_control,
    RowSchema,
};

pub(super) fn resolve(
    context: ViewRewriteContext<'_>,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<ViewMutationTarget>, SQLError> {
    let canonical = context
        .catalog
        .try_resolve_view_name_with_control(name, control)
        .map_err(|error| match error {
            SQLError::Internal(message) => {
                SQLError::Internal(format!("resolve DML view `{name}`: {message}"))
            }
            error => error,
        })?
        .ok_or_else(|| SQLError::UnknownTable(name.to_owned()))?;
    let mut target = None;
    context.authorization.with_view_definition_with_control(
        &canonical,
        control,
        &mut |definition| {
            let definition = definition.ok_or_else(|| SQLError::UnknownTable(name.to_owned()))?;
            if definition.kind != StoredViewKind::View {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("relation \"{}\" is not a view", canonical.as_str()),
                });
            }
            let schema = stored_view_schema_with_control(
                context,
                definition.definition.rewrite_definition_ref(),
                control,
            )?;
            target = Some(capture(&canonical, definition, schema, control)?);
            Ok(())
        },
    )?;
    target.ok_or_else(|| {
        SQLError::Internal("view privilege catalog did not supply its definition".into())
    })
}

fn capture(
    canonical: &str,
    definition: &StoredView,
    schema: RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Produced<ViewMutationTarget>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let canonical_name = output.text(canonical)?;
    let mut columns = output.vector(schema.len())?;
    for (position, column) in schema.columns().iter().enumerate() {
        columns.push(output.text(schema.public_name(position).unwrap_or(column))?)?;
    }
    let target = ViewMutationTarget {
        canonical_name,
        query: output.take(definition.query.clone_with_control(control)?),
        security: definition.security.capture_privileges(control)?,
        security_invoker: definition.security_invoker(),
        schema,
        columns: columns.finish()?,
    };
    output.finish(target)
}

#[cfg(test)]
mod tests;
