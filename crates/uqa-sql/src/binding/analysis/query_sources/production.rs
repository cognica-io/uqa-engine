//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retrieval namespace analysis borrows qualifier names and admits only its selected indexes.

use crate::{
    plan::{ProjectionPlan, QueryBlockPlan},
    ColumnType, RowSchema, SQLError, ScalarExpr,
};
use uqa_core::memory::{ProductionControl, ProductionMap};

pub(in crate::binding) fn with_query_source_columns_with_control(
    schema: &RowSchema,
    block: &QueryBlockPlan,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let schema = crate::binding::with_query_table_pseudo_columns_with_control(schema, control)?;
    let (Some(source), Some(predicate)) = (&block.from, &block.r#where) else {
        return Ok(schema);
    };
    let mut nullable = ProductionMap::new(*control);
    crate::semantics::visit_outer_join_nullable_qualifiers_with_control(
        source,
        control,
        &mut |qualifier| {
            nullable.insert_copy(qualifier, ())?;
            Ok(())
        },
    )?;
    let mut scored = ProductionMap::new(*control);
    crate::semantics::visit_filter_parts_with_control(predicate, control, &mut |part| {
        if !crate::semantics::contains_retrieval(part) || part.contains_subquery() {
            return Ok(());
        }
        let mut qualifiers = ProductionMap::new(*control);
        part.try_visit(&mut |expression| {
            control.check()?;
            if let Some(qualifier) = expression.reference_qualifier() {
                qualifiers.insert_copy(qualifier, ())?;
            }
            if let ScalarExpr::Column(column) = expression {
                for identity in schema.identities() {
                    control.check()?;
                    if identity.column() == column {
                        if let Some(qualifier) = identity.qualifier() {
                            qualifiers.insert_copy(qualifier, ())?;
                        }
                    }
                }
            }
            Ok::<_, SQLError>(true)
        })?;
        if qualifiers.len() == 1 {
            let (qualifier, ()) = qualifiers.iter().next().expect("one qualifier");
            if nullable.get(qualifier).is_none() && schema.has_qualified_column(qualifier, "_score")
            {
                scored.insert_copy(*qualifier, ())?;
            }
        }
        Ok(())
    })?;
    if scored.len() != 1 {
        return Ok(schema);
    }
    RowSchema::with_virtual_columns_with_control(
        &schema,
        [(None, "_score", Some(&ColumnType::DoublePrecision))],
        control,
    )
}

pub(in crate::binding) fn with_projected_open_columns_with_control(
    output: &RowSchema,
    projections: &[ProjectionPlan],
    source: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    for projection in projections {
        control.check()?;
        let open = match &projection.expr {
            ScalarExpr::Star => source.columns_are_open(None),
            ScalarExpr::QualifiedStar(qualifier) => source.columns_are_open(Some(qualifier)),
            _ => false,
        };
        if open {
            return RowSchema::with_open_columns_with_control(output, None, control);
        }
    }
    Ok(output.clone())
}
