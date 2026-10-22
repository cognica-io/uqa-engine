//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Outer-scope composition and common row-value typing.

use crate::ast::ColumnType;
use crate::plan::{ExpressionPlan, QueryPlan};
use crate::RowSchema;
use crate::{SQLError, SQLParam};

use super::{BindingContext, ScalarExpr, SchemaScope};
use crate::routines::RoutineResolution;

/// Derive the exact output row type of a query plan without executing it.
pub fn bind_query_plan_schema(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context(ctes)?.bind_query(routines, plan, params, outer)
}

/// Derive the declared SQL type of a standalone expression plan without executing it. The plan-owned subquery arena participates in type resolution so scalar subqueries retain their projected type at command boundaries such as `CALL`.
pub fn bind_expression_plan_type(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<Option<ColumnType>, SQLError> {
    SchemaScope::from_context(ctes)?.bind_expression_type(
        routines,
        &plan.scalar,
        &RowSchema::default(),
        &plan.subqueries,
        params,
    )
}

/// Validate a command argument's names and types without running its expressions.
pub fn analyze_expression_plan_type(
    routines: &dyn RoutineResolution,
    plan: &ExpressionPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<Option<ColumnType>, SQLError> {
    SchemaScope::for_analysis(ctes)?.bind_expression_type(
        routines,
        &plan.scalar,
        &RowSchema::default(),
        &plan.subqueries,
        params,
    )
}

/// Analyze every catalog and scalar reference and derive the exact output row type without executing the query.
pub fn analyze_query_plan_schema(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    ctes: &BindingContext,
    outer: Option<&RowSchema>,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_analysis(ctes)?.bind_query(routines, plan, params, outer)
}

pub fn analyze_query_plan_schema_with_catalog(
    routines: &dyn RoutineResolution,
    plan: &QueryPlan,
    params: &[SQLParam],
    catalog: crate::catalog::analysis::CatalogReadView,
    resolution: crate::catalog::resolution::RelationNameResolution,
) -> Result<RowSchema, SQLError> {
    SchemaScope::for_catalog_analysis(catalog, resolution).bind_query(routines, plan, params, None)
}

pub fn overlay_outer_schema(current: &RowSchema, outer: Option<&RowSchema>) -> RowSchema {
    outer.map_or_else(
        || current.clone(),
        |outer| RowSchema::with_outer_schema(current, outer),
    )
}

/// Lexical ancestors and preceding FROM items are different namespaces. Only a lateral range item activates its preceding peers; a nested join carries them to either operand without changing their query level.
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct SourceScope<'a> {
    pub outer: Option<&'a RowSchema>,
    pub preceding: Option<&'a RowSchema>,
    pub inherited_lateral: bool,
    pub retain_names: bool,
}

impl<'a> SourceScope<'a> {
    pub fn describe_output(source: &crate::plan::SourcePlan, schema: RowSchema) -> RowSchema {
        if matches!(
            source,
            crate::plan::SourcePlan::Join { alias: None, .. }
                | crate::plan::SourcePlan::RetainedRows(_)
        ) {
            schema
        } else {
            RowSchema::with_source_diagnostic_namespace(&schema, source_namespace(source))
        }
    }

    pub fn with_names(self) -> Self {
        Self {
            retain_names: true,
            ..self
        }
    }

    pub fn command_target(
        command: &crate::plan::CommandPlan,
        target: &RowSchema,
    ) -> Option<RowSchema> {
        matches!(
            command,
            crate::plan::CommandPlan::Update(_) | crate::plan::CommandPlan::Delete(_)
        )
        .then(|| {
            let namespace = command
                .target_qualifier()
                .expect("mutation target qualifier");
            let target = RowSchema::with_relation_qualifier(target, namespace);
            let target = RowSchema::with_source_diagnostic_namespace(&target, namespace);
            RowSchema::without_target_lateral_access(&target, namespace)
        })
    }

    pub fn new(outer: Option<&'a RowSchema>) -> Self {
        Self {
            outer,
            preceding: None,
            inherited_lateral: false,
            retain_names: false,
        }
    }

    pub fn input(self, source: &crate::plan::SourcePlan) -> Option<RowSchema> {
        use crate::plan::SourcePlan;
        let Some(preceding) = self.preceding else {
            return self.outer.cloned();
        };
        let active = self.inherited_lateral
            || matches!(
                source,
                SourcePlan::Function { .. }
                    | SourcePlan::FunctionGroup { .. }
                    | SourcePlan::Values { lateral: true, .. }
                    | SourcePlan::Subquery { lateral: true, .. }
            );
        Some(if active {
            overlay_outer_schema(preceding, self.outer)
        } else {
            RowSchema::with_unavailable_names(
                &self.outer.cloned().unwrap_or_default(),
                preceding,
                true,
            )
        })
    }

    pub fn right_namespace(
        self,
        source: &crate::plan::SourcePlan,
        left: &RowSchema,
        kind: crate::ast::JoinKind,
    ) -> RowSchema {
        let left = lateral_join_scope(source, left, None, kind);
        self.preceding.map_or_else(
            || left.clone(),
            |preceding| RowSchema::with_peer_scope(preceding, &left),
        )
    }

    pub fn condition(self) -> Option<RowSchema> {
        self.preceding.map_or_else(
            || self.outer.cloned(),
            |preceding| {
                Some(RowSchema::with_unavailable_names(
                    &self.outer.cloned().unwrap_or_default(),
                    preceding,
                    false,
                ))
            },
        )
    }
}

pub(super) fn lateral_join_scope(
    source: &crate::plan::SourcePlan,
    left: &RowSchema,
    outer: Option<&RowSchema>,
    kind: crate::ast::JoinKind,
) -> RowSchema {
    if matches!(
        kind,
        crate::ast::JoinKind::Right | crate::ast::JoinKind::Full
    ) {
        let namespace = source_namespace(source);
        overlay_outer_schema(&RowSchema::without_lateral_access(left, namespace), outer)
    } else {
        overlay_outer_schema(left, outer)
    }
}

pub fn values_types_in_scope(
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    SchemaScope::from_context(ctes)?.bind_values_types(routines, rows, subqueries, schema, params)
}

/// Preserve the complete analyzed VALUES output metadata for physical execution.
pub fn values_schema_in_scope(
    routines: &dyn RoutineResolution,
    rows: &[Vec<ScalarExpr>],
    subqueries: &[QueryPlan],
    schema: Option<&RowSchema>,
    params: &[SQLParam],
    ctes: &BindingContext,
) -> Result<RowSchema, SQLError> {
    SchemaScope::from_context(ctes)?.bind_values_schema(routines, rows, subqueries, schema, params)
}

fn source_namespace(source: &crate::plan::SourcePlan) -> &str {
    use crate::plan::SourcePlan;
    // These diagnostic range-entry names do not expose a SQL qualifier.
    match source {
        SourcePlan::Table {
            qualifier, alias, ..
        } => alias.as_deref().unwrap_or(qualifier),
        SourcePlan::Function {
            output_name, alias, ..
        } => alias.as_deref().unwrap_or(output_name),
        SourcePlan::FunctionGroup {
            functions, alias, ..
        } => alias.as_deref().unwrap_or_else(|| {
            &functions
                .first()
                .expect("bound ROWS FROM source has a member")
                .output_name
        }),
        SourcePlan::Join { alias, .. } => alias.as_deref().unwrap_or("unnamed_join"),
        SourcePlan::Values { alias, .. } | SourcePlan::Subquery { alias, .. } => {
            alias.as_deref().unwrap_or("unnamed_subquery")
        }
        SourcePlan::RetainedRows(_) => "unnamed_subquery",
    }
}
