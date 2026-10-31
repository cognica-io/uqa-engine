//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog and name analysis for execution-free query schema binding.

mod calls;
mod functions;
mod query_sources;
mod references;
#[cfg(test)]
mod tests;
mod windows;

pub(super) use calls::validate_call;
pub(super) use functions::validate_window_function;
pub(super) use query_sources::with_query_source_columns;
pub(super) use query_sources::{
    with_projected_open_columns_with_control, with_query_source_columns_with_control,
};
pub(super) use windows::references_local_column;

use super::type_resolution::ValidatedSubqueries;
use super::{QueryBlockPlan, QueryPlan, SQLError, SQLParam, ScalarExpr, SchemaScope};
use crate::ast::{ColumnType, FunctionBinding};
use crate::routines::RoutineResolution;
use crate::{FunctionTypeResolver, RowSchema};

pub(super) struct TableFunctionSourceValidation<'a> {
    pub(super) name: &'a str,
    pub(super) binding: Option<&'a FunctionBinding>,
    pub(super) args: &'a [ScalarExpr],
    pub(super) subqueries: &'a [QueryPlan],
    pub(super) input: &'a RowSchema,
    pub(super) params: &'a [SQLParam],
}

/// A LIMIT or OFFSET argument with the scopes it is validated against.
struct LimitArgument<'a> {
    expression: &'a ScalarExpr,
    construct: &'static str,
    /// The block's own columns over the enclosing scope.
    source: &'a RowSchema,
    enclosing: &'a RowSchema,
}

struct AliasReferenceScope<'a> {
    primary: &'a RowSchema,
    fallback: &'a RowSchema,
    nested: &'a RowSchema,
}

impl SchemaScope {
    /// `DISTINCT ON` and `ORDER BY` items name an output column before a column of the block's source.
    fn validate_output_references(
        &mut self,
        queries: &ValidatedSubqueries<'_, '_>,
        block: &QueryBlockPlan,
        output: &RowSchema,
        source: &RowSchema,
    ) -> Result<(), SQLError> {
        for expression in block
            .distinct_on
            .iter()
            .chain(block.order_by.iter().map(|order| &order.expr))
        {
            self.validate_alias_reference(
                queries,
                expression,
                AliasReferenceScope {
                    primary: output,
                    fallback: source,
                    nested: source,
                },
            )?;
        }
        Ok(())
    }

    /// Check a query block's clauses in `transformSelectStmt`'s order after its target list, against `source`, the block's own columns over `outer`, the scope of the queries that enclose it: `WHERE`, `GROUP BY`, `HAVING`, `DISTINCT ON`, `ORDER BY`, `LIMIT` and `OFFSET`, then its window frames.
    pub(super) fn validate_query_block_clauses(
        &mut self,
        engine: &dyn RoutineResolution,
        block: &QueryBlockPlan,
        source: &RowSchema,
        output: &RowSchema,
        params: &[SQLParam],
        outer: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let queries = ValidatedSubqueries::new(engine, &block.subqueries, params).retaining();
        if let Some(predicate) = block.r#where.as_ref() {
            self.validate_condition_references(&queries, predicate, "WHERE", source)?;
        }
        for expression in block
            .group_by
            .iter()
            .chain(block.grouping_sets.iter().flatten())
        {
            let expression = crate::semantics::grouping_sets::resolve_grouping_expression(
                engine,
                expression,
                &block.projections,
                source,
                outer,
                params,
            )?;
            self.validate_expression_references_with_queries(&queries, &expression, source, None)?;
        }
        if let Some(having) = block.having.as_ref() {
            self.validate_condition_references(&queries, having, "HAVING", source)?;
        }
        self.validate_output_references(&queries, block, output, source)?;
        let enclosing = outer.cloned().unwrap_or_default();
        for (expression, construct) in block
            .limit
            .iter()
            .map(|expression| (expression, "LIMIT"))
            .chain(block.offset.iter().map(|expression| (expression, "OFFSET")))
        {
            self.validate_limit_argument(
                &queries,
                LimitArgument {
                    expression,
                    construct,
                    source,
                    enclosing: &enclosing,
                },
            )?;
        }
        self.validate_window_frames(
            block,
            &windows::WindowFrameScope {
                source,
                queries: &queries,
            },
        )?;
        crate::semantics::grouping_sets::validate_grouped_expressions(
            engine, block, source, outer, params,
        )
    }

    /// Validate a LIMIT or OFFSET argument, which can name the columns of the enclosing queries and the parameters but no column of its own query level, as `checkExprIsVarFree` requires.
    fn validate_limit_argument(
        &mut self,
        queries: &ValidatedSubqueries<'_, '_>,
        argument: LimitArgument<'_>,
    ) -> Result<(), SQLError> {
        let Err(error) = self.validate_expression_references_with_queries(
            queries,
            argument.expression,
            argument.enclosing,
            None,
        ) else {
            return Ok(());
        };
        if self
            .validate_expression_references_with_queries(
                queries,
                argument.expression,
                argument.source,
                None,
            )
            .is_ok()
        {
            return Err(SQLError::Routine {
                sqlstate: "42P10".into(),
                message: format!(
                    "argument of {} must not contain variables",
                    argument.construct
                ),
            });
        }
        Err(error)
    }

    fn validate_alias_reference(
        &mut self,
        queries: &ValidatedSubqueries<'_, '_>,
        expression: &ScalarExpr,
        scope: AliasReferenceScope<'_>,
    ) -> Result<(), SQLError> {
        if matches!(expression, ScalarExpr::Column(_) | ScalarExpr::Position(_)) {
            self.validate_expression_references_with_queries(
                queries,
                expression,
                scope.primary,
                Some(scope.fallback),
            )
        } else {
            self.validate_expression_references_with_queries(
                queries,
                expression,
                scope.nested,
                None,
            )
        }
    }

    pub(super) fn validate_expression_references(
        &mut self,
        engine: &dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        fallback: Option<&RowSchema>,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        self.validate_expression_references_with_queries(
            &ValidatedSubqueries::new(engine, subqueries, params),
            expression,
            schema,
            fallback,
        )
    }

    fn validate_expression_references_with_queries(
        &mut self,
        queries: &ValidatedSubqueries<'_, '_>,
        expression: &ScalarExpr,
        schema: &RowSchema,
        fallback: Option<&RowSchema>,
    ) -> Result<(), SQLError> {
        let schema = self.with_stored_outer_internal_aliases(schema)?;
        let resolver = queries.resolver(self, std::slice::from_ref(expression), &schema)?;
        Self::validate_expression_references_with_resolver(
            self.rule_condition_event,
            queries.routines,
            expression,
            &schema,
            fallback,
            queries.params,
            &resolver,
        )
    }

    /// A clause condition must be boolean, as `transformWhereClause` requires.
    pub(super) fn validate_condition(
        &mut self,
        engine: &dyn RoutineResolution,
        condition: &ScalarExpr,
        construct: &str,
        schema: &RowSchema,
        subqueries: &[QueryPlan],
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        let schema = self.with_stored_outer_internal_aliases(schema)?;
        super::preparation::condition_metadata(
            self,
            engine,
            condition,
            construct,
            params,
            &super::type_resolution::BoundConditionQueries {
                plans: subqueries,
                input: &schema,
                schemas: None,
            },
        )
    }

    /// Reuse only successful subquery bindings from this condition's reference check. Its Boolean coercion still follows reference errors, and no result leaves this lexical scope.
    fn validate_condition_references(
        &mut self,
        queries: &ValidatedSubqueries<'_, '_>,
        condition: &ScalarExpr,
        construct: &str,
        schema: &RowSchema,
    ) -> Result<(), SQLError> {
        let schema = self.with_stored_outer_internal_aliases(schema)?;
        let resolver = queries.resolver(self, std::slice::from_ref(condition), &schema)?;
        Self::validate_expression_references_with_resolver(
            self.rule_condition_event,
            queries.routines,
            condition,
            &schema,
            None,
            queries.params,
            &resolver,
        )?;
        super::preparation::condition_metadata(
            self,
            queries.routines,
            condition,
            construct,
            queries.params,
            &super::type_resolution::BoundConditionQueries {
                plans: queries.plans,
                input: &schema,
                schemas: resolver.scalar_subquery_schemas.as_deref(),
            },
        )
    }

    pub(super) fn validate_expression_references_with_resolver(
        rule_event: Option<crate::ast::RuleEvent>,
        engine: &dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        fallback: Option<&RowSchema>,
        params: &[SQLParam],
        resolver: &dyn FunctionTypeResolver,
    ) -> Result<(), SQLError> {
        references::validate_expression(
            rule_event, engine, expression, schema, fallback, params, resolver,
        )
    }

    pub(super) fn validate_table_function_source(
        &mut self,
        engine: &dyn RoutineResolution,
        request: TableFunctionSourceValidation<'_>,
    ) -> Result<Option<crate::semantics::ControlledUserTableFunction>, SQLError> {
        let TableFunctionSourceValidation {
            name,
            binding,
            args,
            subqueries,
            input,
            params,
        } = request;
        let resolver = self
            .query_function_type_resolver_for_subqueries(engine, args, input, subqueries, params)?;
        for argument in args {
            Self::validate_expression_references_with_resolver(
                self.rule_condition_event,
                engine,
                argument,
                input,
                None,
                params,
                &resolver,
            )?;
        }
        functions::validate_table_function(
            crate::semantics::TableFunctionAnalysis {
                routines: engine,
                types: &resolver,
            },
            name,
            binding,
            args,
            input,
            params,
            &self.control(),
        )
    }
}

pub(super) fn with_table_pseudo_columns_with_control(
    schema: &RowSchema,
    qualifier: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    RowSchema::with_virtual_columns_with_control(
        schema,
        [
            (Some(qualifier), "_doc_id", Some(&ColumnType::BigInteger)),
            (
                Some(qualifier),
                "_score",
                Some(&ColumnType::DoublePrecision),
            ),
            (Some(qualifier), "tableoid", Some(&ColumnType::Oid)),
            (Some(qualifier), "xmin", Some(&ColumnType::Xid)),
        ],
        control,
    )
}
