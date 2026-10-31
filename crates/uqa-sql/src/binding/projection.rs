//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection row-type binding and reference validation.

use crate::ast::ColumnType;
use crate::plan::{ProjectionPlan, QueryBlockPlan, QueryPlan};
use crate::RowSchema;
use crate::{SQLError, SQLParam};

use super::production::ControlledBindingContext;
use super::{BindingContext, QueryFunctionTypeResolver, ScalarExpr, SchemaScope};
use crate::routines::RoutineResolution;
use uqa_core::memory::ProductionControl;

#[cfg(test)]
mod tests;

mod output;
pub(super) use output::ProjectionOutput;

pub(super) struct ProjectionMetadata {
    pub ty: Option<uqa_core::memory::Produced<ColumnType>>,
    pub collation: Option<u32>,
    pub record: Option<crate::schema::RecordMetadata>,
}

/// The declared scalar-expression scope and the local relation expanded by a bare wildcard.
#[derive(Clone, Copy)]
pub struct ProjectionSchemas<'a> {
    pub expressions: &'a RowSchema,
    pub star: &'a RowSchema,
}

/// Bind a projection against an already-declared input schema. `star_schema` identifies the relation expanded by bare `*`; `expression_schema` may also contain joined sources and hidden lookup aliases used by scalar expressions.
pub fn bind_projection_output_schema(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<RowSchema, SQLError> {
    let control = ProductionControl::uncontrolled();
    bind_projection_output_schema_with_control(
        routines,
        projections,
        ProjectionSchemas {
            expressions: expression_schema,
            star: star_schema,
        },
        subqueries,
        params,
        ctes.copy_with_control(&control)?,
        &control,
    )
}

/// Bind projection metadata under the invoking request's allowance.
pub fn bind_projection_output_schema_with_control(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    schemas: ProjectionSchemas<'_>,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    projection_output_schema(
        SchemaScope::from_context_with_control(ctes, control)?,
        routines,
        projections,
        schemas.expressions,
        schemas.star,
        subqueries,
        params,
    )
}

/// Derive and validate a projection's exact output row type without executing it.
pub fn analyze_projection_output_schema(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<RowSchema, SQLError> {
    let control = ProductionControl::uncontrolled();
    analyze_projection_output_schema_with_control(
        routines,
        projections,
        ProjectionSchemas {
            expressions: expression_schema,
            star: star_schema,
        },
        subqueries,
        params,
        ctes.copy_with_control(&control)?,
        &control,
    )
}

/// Analyze projection metadata with admission before allocating expressions and row descriptors.
pub fn analyze_projection_output_schema_with_control(
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    schemas: ProjectionSchemas<'_>,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
    ctes: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    projection_output_schema(
        SchemaScope::for_analysis_with_control(ctes, control)?,
        routines,
        projections,
        schemas.expressions,
        schemas.star,
        subqueries,
        params,
    )
}

fn projection_output_schema(
    mut scope: SchemaScope,
    routines: &dyn RoutineResolution,
    projections: &[ProjectionPlan],
    expression_schema: &RowSchema,
    star_schema: &RowSchema,
    subqueries: &[QueryPlan],
    params: &[SQLParam],
) -> Result<RowSchema, SQLError> {
    let owner = scope.control().capture();
    let control = owner.control();
    let labels = crate::semantics::projection_labels_with_control(projections, &[], &control)?;
    let mut output = ProjectionOutput::new(control);
    for (label, projection) in labels.iter().zip(projections) {
        let expansion_schema = match projection.expr {
            ScalarExpr::QualifiedStar(_) => expression_schema,
            _ => star_schema,
        };
        if output.star(&projection.expr, expansion_schema)? {
            continue;
        }
        let metadata = scope.bind_projection_metadata(
            routines,
            &projection.expr,
            expression_schema,
            subqueries,
            params,
        )?;
        output.push(label, metadata)?;
    }
    for projection in projections {
        scope.collations.validate_expression(&projection.expr)?;
    }
    output.finish()
}

/// Validate every scalar expression in a query block while the physical input still carries declared SQL types. This must precede polymorphic rewrites such as `pg_typeof`, because an invalid common type is an error, not an `unknown` result.
pub fn validate_query_block_expression_types(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<(), SQLError> {
    let control = ProductionControl::uncontrolled();
    validate_query_block_expression_types_with_control(
        routines,
        statement,
        schema,
        params,
        ctes.copy_with_control(&control)?,
        &control,
    )
}

/// Preserve expression and comparison-check order while every inferred type retains its allocation owner.
pub fn validate_query_block_expression_types_with_control(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: ControlledBindingContext<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    let mut scalar_subquery_schemas = crate::production::Buffer::new(control);
    for plan in &statement.subqueries {
        scalar_subquery_schemas.push(
            Some(super::production::bind_query_plan_schema_with_control(
                routines,
                plan,
                params,
                ctes.copy_with_control(control)?,
                Some(schema),
                control,
            )?),
            control,
        )?;
    }
    let resolver = QueryFunctionTypeResolver {
        routines,
        scalar_subquery_schemas: Some(scalar_subquery_schemas),
        defer_routine_namespace_errors: true,
    };
    let resolve = |expression| {
        crate::scalar_type_with_resolver_and_control(
            expression,
            schema,
            params,
            Some(&resolver),
            control,
        )
    };
    for projection in &statement.projections {
        resolve(&projection.expr)?;
    }
    let mut equality_types = crate::production::Buffer::new(control);
    for expression in statement
        .group_by
        .iter()
        .chain(statement.grouping_sets.iter().flatten())
    {
        equality_types.push(resolve(expression)?, control)?;
    }
    let mut ordering_types = crate::production::Buffer::new(control);
    for order in &statement.order_by {
        ordering_types.push(resolve(&order.expr)?, control)?;
    }
    for expression in &statement.distinct_on {
        equality_types.push(resolve(expression)?, control)?;
    }
    for expression in statement
        .r#where
        .iter()
        .chain(statement.having.iter())
        .chain(statement.limit.iter())
        .chain(statement.offset.iter())
    {
        resolve(expression)?;
    }
    // All expression type errors still precede capability checks. These selections belong only to this invocation's immutable input scope.
    for ty in equality_types.iter().flatten() {
        crate::type_resolution::ComparisonSupport::Equality.require_in_catalog(ty, routines)?;
    }
    for ty in ordering_types.iter().flatten() {
        crate::type_resolution::ComparisonSupport::Ordering.require_in_catalog(ty, routines)?;
    }
    Ok(())
}

/// Validate every query-block reference only after the caller has the authoritative source schema. This preserves registered table-function row shapes and checks recursive argument references before definitive routine namespace lookup. `schema` holds the block's columns over `outer`, the scope of the queries that enclose the block.
pub fn validate_query_block_references(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<(), SQLError> {
    let control = ProductionControl::uncontrolled();
    validate_query_block_references_with_control(
        routines,
        statement,
        schema,
        params,
        ctes.copy_with_control(&control)?,
        outer,
        &control,
    )
}

/// Validate references with the authoritative source schema and the request's production control.
pub fn validate_query_block_references_with_control(
    routines: &dyn RoutineResolution,
    statement: &QueryBlockPlan,
    schema: &RowSchema,
    params: &[SQLParam],
    ctes: ControlledBindingContext<'_>,
    outer: Option<&RowSchema>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let output = analyze_projection_output_schema_with_control(
        routines,
        &statement.projections,
        ProjectionSchemas {
            expressions: schema,
            star: schema,
        },
        &statement.subqueries,
        params,
        ctes.copy_with_control(control)?,
        control,
    )?;
    if statement.distinct && statement.distinct_on.is_empty() {
        for ty in output.column_types().iter().flatten() {
            crate::type_resolution::ComparisonSupport::Equality.require_in_catalog(ty, routines)?;
        }
    }
    SchemaScope::for_analysis_with_control(ctes, control)?
        .validate_query_block_clauses(routines, statement, schema, &output, params, outer)
}

pub(super) fn visit_projection_star_metadata<'a>(
    expression: &'a ScalarExpr,
    schema: &'a RowSchema,
    control: &uqa_core::memory::ProductionControl<'_>,
    observe: &mut dyn FnMut(crate::semantics::ProjectionStarMetadata<'a>) -> Result<(), SQLError>,
) -> Result<bool, SQLError> {
    crate::semantics::visit_bound_projection_star_metadata(
        expression, schema, schema, control, observe,
    )
    .map_err(|error| error.at_position(expression.reference_position()))
}

pub(super) fn rename_schema(
    schema: &RowSchema,
    aliases: &[String],
    qualifier: Option<&str>,
) -> RowSchema {
    rename_schema_with_control(
        schema,
        aliases,
        qualifier,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .expect("ordinary result renaming")
}

pub(super) fn rename_schema_with_control(
    schema: &RowSchema,
    aliases: &[String],
    qualifier: Option<&str>,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    RowSchema::renamed_with_control(schema, aliases, qualifier, control)
}
