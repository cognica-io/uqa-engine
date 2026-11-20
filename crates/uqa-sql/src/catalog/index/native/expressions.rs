//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read stored index expressions against their original row and routine declarations.

use crate::catalog::scalar_bindings::{
    expression_type_with_work, validate_boolean_with_work, NativeScalarContext,
};
use crate::{
    ast::{ColumnDef, IndexKey},
    binding::production::ControlledBindingContext,
    catalog::index::IndexDefinition,
    routines::{native::NativeRoutineReferences, RoutineResolution},
    ColumnType, SQLError,
};
use uqa_core::memory::ProductionControl;

pub struct IndexBindingContext<'a, 'b> {
    pub relation: Option<&'a crate::RelationIdentity>,
    pub row_type: Option<&'a ColumnType>,
    pub routines: &'a dyn RoutineResolution,
    pub references: &'a NativeRoutineReferences<'b>,
    pub types: &'a crate::catalog::type_references::TypeReferences<'b>,
    pub binding: &'a ControlledBindingContext<'b>,
}

/// Validate the original key result types and predicate without rebuilding an index, changing a binding, evaluating a scalar or compiling a routine body. Empty legacy type vectors remain absent; document fields without declarations retain their open row scope.
pub fn validate_bindings(
    keys: &[IndexKey],
    definition: &IndexDefinition,
    columns: Option<&[ColumnDef]>,
    context: &IndexBindingContext<'_, '_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    crate::catalog::work::inline(validate_bindings_with_work(
        keys,
        definition,
        columns,
        context,
        control,
        &mut (),
    ))
}

pub async fn validate_bindings_with_work(
    keys: &[IndexKey],
    definition: &IndexDefinition,
    columns: Option<&[ColumnDef]>,
    context: &IndexBindingContext<'_, '_>,
    control: &ProductionControl<'_>,
    work: &mut (impl crate::catalog::CatalogWork + Send),
) -> Result<(), SQLError> {
    control.check()?;
    if !definition.key_types.is_empty() && definition.key_types.len() != keys.len() {
        return Err(invalid("key type count"));
    }
    for ty in &definition.key_types {
        work.checkpoint().await?;
        context.types.column_type_with_work(ty, work).await?;
    }
    let schema = crate::schema::indexes::row_schema_with_control(
        context.relation.map(|relation| relation.name.as_str()),
        columns,
        context.row_type,
        control,
    )?;
    let scalar = NativeScalarContext {
        subject: "index",
        routines: context.routines,
        references: context.references,
        types: context.types,
        binding: context.binding,
    };
    for (position, key) in keys.iter().enumerate() {
        work.checkpoint().await?;
        control.check()?;
        let expected = definition.key_types.get(position);
        match key {
            IndexKey::Column(name) => {
                for column in columns.into_iter().flatten() {
                    work.checkpoint().await?;
                    if crate::catalog::work::compare_text(work, &column.name, name, control)
                        .await?
                        .is_eq()
                    {
                        if let Some(expected) = expected {
                            if !crate::type_resolution::same_declaration_with_work(
                                expected,
                                &column.ty,
                                control,
                                &mut crate::catalog::work::ReferenceWork { control, work },
                            )
                            .await?
                            {
                                return Err(invalid("column key type"));
                            }
                        }
                        break;
                    }
                }
            }
            IndexKey::Expression(expression) => {
                let ty =
                    expression_type_with_work(expression, &schema, &scalar, control, work).await?;
                // Ordinary index declaration resolves an unknown result to text.
                if let Some(expected) = expected {
                    if !crate::type_resolution::same_declaration_with_work(
                        expected,
                        ty.as_deref().unwrap_or(&ColumnType::Text),
                        control,
                        &mut crate::catalog::work::ReferenceWork { control, work },
                    )
                    .await?
                    {
                        return Err(invalid("expression key type"));
                    }
                }
            }
        }
    }
    if let Some(predicate) = &definition.predicate {
        validate_boolean_with_work(predicate, &schema, &scalar, control, work).await?;
    }
    control.check()?;
    Ok(())
}

fn invalid(detail: &str) -> SQLError {
    SQLError::Internal(format!("invalid native index binding: {detail}"))
}

#[cfg(test)]
mod tests;
