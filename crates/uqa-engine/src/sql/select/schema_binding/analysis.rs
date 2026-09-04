//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog and name analysis for execution-free query schema binding.

mod functions;
mod references;

use super::{QueryBlockPlan, QueryPlan, SQLError, SQLParam, ScalarExpr, SchemaScope};
use crate::engine_user_functions::RoutineResolution;
use crate::sql::{META_DOC_ID_COLUMN, META_QUALIFIER, META_SCORE_COLUMN};
use uqa_execution::{ColumnIdentity, FunctionTypeResolver, RowSchema};
use uqa_sql::ast::{ColumnType, FunctionBinding};

pub(super) struct SetOperationClauses<'a> {
    pub(super) order_by: &'a [uqa_planner::OrderPlan],
    pub(super) limit: Option<&'a ScalarExpr>,
    pub(super) offset: Option<&'a ScalarExpr>,
    pub(super) subqueries: &'a [QueryPlan],
    pub(super) output: &'a RowSchema,
}

pub(super) struct TableFunctionSourceValidation<'a> {
    pub(super) name: &'a str,
    pub(super) binding: Option<&'a FunctionBinding>,
    pub(super) args: &'a [ScalarExpr],
    pub(super) subqueries: &'a [QueryPlan],
    pub(super) input: &'a RowSchema,
    pub(super) params: &'a [SQLParam],
}

struct AliasReferenceScope<'a> {
    primary: &'a RowSchema,
    fallback: &'a RowSchema,
    nested: &'a RowSchema,
    subqueries: &'a [QueryPlan],
    params: &'a [SQLParam],
}

impl SchemaScope {
    pub(super) fn validate_query_block_clauses(
        &mut self,
        engine: &dyn RoutineResolution,
        block: &QueryBlockPlan,
        source: &RowSchema,
        output: &RowSchema,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        if let Some(predicate) = block.r#where.as_ref() {
            self.validate_expression_references(
                engine,
                predicate,
                source,
                None,
                &block.subqueries,
                params,
            )?;
        }
        for expression in block
            .group_by
            .iter()
            .chain(block.grouping_sets.iter().flatten())
        {
            self.validate_alias_reference(
                engine,
                expression,
                AliasReferenceScope {
                    primary: source,
                    fallback: output,
                    nested: source,
                    subqueries: &block.subqueries,
                    params,
                },
            )?;
        }
        if let Some(having) = block.having.as_ref() {
            self.validate_expression_references(
                engine,
                having,
                source,
                None,
                &block.subqueries,
                params,
            )?;
        }
        for expression in &block.distinct_on {
            self.validate_alias_reference(
                engine,
                expression,
                AliasReferenceScope {
                    primary: output,
                    fallback: source,
                    nested: source,
                    subqueries: &block.subqueries,
                    params,
                },
            )?;
        }
        for order in &block.order_by {
            self.validate_alias_reference(
                engine,
                &order.expr,
                AliasReferenceScope {
                    primary: output,
                    fallback: source,
                    nested: source,
                    subqueries: &block.subqueries,
                    params,
                },
            )?;
        }
        let empty = RowSchema::default();
        for expression in block.limit.iter().chain(block.offset.iter()) {
            self.validate_expression_references(
                engine,
                expression,
                &empty,
                None,
                &block.subqueries,
                params,
            )?;
        }
        Ok(())
    }

    pub(super) fn validate_set_operation_clauses(
        &mut self,
        engine: &dyn RoutineResolution,
        clauses: SetOperationClauses<'_>,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        for order in clauses.order_by {
            self.validate_expression_references(
                engine,
                &order.expr,
                clauses.output,
                None,
                clauses.subqueries,
                params,
            )?;
        }
        let empty = RowSchema::default();
        for expression in clauses.limit.into_iter().chain(clauses.offset) {
            self.validate_expression_references(
                engine,
                expression,
                &empty,
                None,
                clauses.subqueries,
                params,
            )?;
        }
        Ok(())
    }

    fn validate_alias_reference(
        &mut self,
        engine: &dyn RoutineResolution,
        expression: &ScalarExpr,
        scope: AliasReferenceScope<'_>,
    ) -> Result<(), SQLError> {
        if matches!(expression, ScalarExpr::Column(_) | ScalarExpr::Position(_)) {
            self.validate_expression_references(
                engine,
                expression,
                scope.primary,
                Some(scope.fallback),
                scope.subqueries,
                scope.params,
            )
        } else {
            self.validate_expression_references(
                engine,
                expression,
                scope.nested,
                None,
                scope.subqueries,
                scope.params,
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
        let schema = self.with_stored_outer_internal_aliases(schema);
        let schema = &schema;
        let resolver = self.query_function_type_resolver(
            engine,
            expression,
            schema,
            subqueries,
            params,
            Some(schema),
        )?;
        Self::validate_expression_references_with_resolver(
            engine, expression, schema, fallback, params, &resolver,
        )
    }

    pub(super) fn validate_expression_references_with_resolver(
        engine: &dyn RoutineResolution,
        expression: &ScalarExpr,
        schema: &RowSchema,
        fallback: Option<&RowSchema>,
        params: &[SQLParam],
        resolver: &dyn FunctionTypeResolver,
    ) -> Result<(), SQLError> {
        references::validate_expression(engine, expression, schema, fallback, params, resolver)
    }

    pub(super) fn validate_table_function_source(
        &mut self,
        engine: &dyn RoutineResolution,
        request: TableFunctionSourceValidation<'_>,
    ) -> Result<Option<crate::sql::from_rows::ResolvedUserTableFunction>, SQLError> {
        let TableFunctionSourceValidation {
            name,
            binding,
            args,
            subqueries,
            input,
            params,
        } = request;
        let resolver = self.query_function_type_resolver_for_subqueries(
            engine,
            args.iter().any(super::expr_contains_subquery),
            input,
            subqueries,
            params,
            Some(input),
        )?;
        for argument in args {
            Self::validate_expression_references_with_resolver(
                engine, argument, input, None, params, &resolver,
            )?;
        }
        functions::validate_table_function(engine, name, binding, args, input, params, &resolver)
    }
}

pub(super) fn with_table_pseudo_columns(schema: &RowSchema, qualifier: &str) -> RowSchema {
    let columns = [
        (
            ColumnIdentity::qualified(qualifier, "_doc_id"),
            Some(ColumnType::BigInteger),
        ),
        (
            ColumnIdentity::qualified(qualifier, "_score"),
            Some(ColumnType::DoublePrecision),
        ),
        (
            ColumnIdentity::qualified(qualifier, "tableoid"),
            Some(ColumnType::Oid),
        ),
        (
            ColumnIdentity::qualified(qualifier, "xmin"),
            Some(ColumnType::Xid),
        ),
    ];
    RowSchema::with_typed_virtual_identities(schema, &columns)
}

pub(super) fn with_unqualified_table_pseudo_columns(schema: &RowSchema) -> RowSchema {
    let Some(qualifier) = functions::single_pseudo_column_qualifier(schema) else {
        return schema.clone();
    };
    let columns = [
        (
            ColumnIdentity::unqualified("_doc_id"),
            schema.qualified_type(&qualifier, "_doc_id").cloned(),
        ),
        (
            ColumnIdentity::unqualified("_score"),
            schema.qualified_type(&qualifier, "_score").cloned(),
        ),
        (
            ColumnIdentity::unqualified("tableoid"),
            schema.qualified_type(&qualifier, "tableoid").cloned(),
        ),
        (
            ColumnIdentity::unqualified("xmin"),
            schema.qualified_type(&qualifier, "xmin").cloned(),
        ),
    ];
    let schema = RowSchema::with_typed_virtual_identities(schema, &columns);
    if schema.has_qualifier(META_QUALIFIER) {
        return schema;
    }
    RowSchema::with_typed_virtual_identities(
        &schema,
        &[
            (
                ColumnIdentity::qualified(META_QUALIFIER, META_DOC_ID_COLUMN),
                Some(ColumnType::BigInteger),
            ),
            (
                ColumnIdentity::qualified(META_QUALIFIER, META_SCORE_COLUMN),
                Some(ColumnType::DoublePrecision),
            ),
        ],
    )
}
