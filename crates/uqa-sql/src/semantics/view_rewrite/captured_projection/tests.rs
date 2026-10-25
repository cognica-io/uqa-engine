//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionBinding, ColumnType, FunctionTypeResolver};

struct Routines;
impl FunctionTypeResolver for Routines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl crate::routines::RoutineResolution for Routines {}

#[test]
fn captured_system_attributes_bind_bare_qualified_and_correlated_names() {
    let source = RowSchema::with_qualified_types(
        "items",
        vec!["id".into()],
        vec![Some(ColumnType::Integer)],
    );
    let source = RowSchema::with_typed_virtual_identities(
        &source,
        &[
            (
                ColumnIdentity::qualified("items", "_doc_id"),
                Some(ColumnType::BigInteger),
            ),
            (
                ColumnIdentity::qualified("items", "_score"),
                Some(ColumnType::DoublePrecision),
            ),
            (
                ColumnIdentity::qualified("items", "tableoid"),
                Some(ColumnType::Oid),
            ),
            (
                ColumnIdentity::qualified("items", "xmin"),
                Some(ColumnType::Xid),
            ),
        ],
    );
    let raw = InternalRelationId::allocate();
    let schema = input_schema(&source, raw);
    let rewrite = metadata_rewrite(&schema, raw);
    let context = crate::binding::fixture::empty_binding_context();
    for (sql, expected) in [
        ("SELECT tableoid", 1),
        ("SELECT items.tableoid", 1),
        ("SELECT xmin", 1),
        ("SELECT (SELECT items.xmin)", 1),
        (
            "SELECT (SELECT tableoid FROM (VALUES (7)) local(tableoid))",
            0,
        ),
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(mut block) = query.root else {
            panic!("block")
        };
        let mut expression = ExpressionPlan {
            scalar: block.projections.remove(0).expr,
            subqueries: block.subqueries,
        };
        crate::binding::bind_expression_plan_routines_for_storage(
            &Routines,
            &mut expression,
            &[],
            &context,
            &schema,
        )
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
        rewrite.bind_expression(&mut expression);
        // Apply deferred names at the scalar query's lexical boundary without changing local columns.
        for query in &mut expression.subqueries {
            let RelationalPlan::QueryBlock(block) = &mut query.root else {
                panic!("scalar block")
            };
            let local = block
                .from
                .as_ref()
                .map(|source| {
                    crate::binding::analyze_source_plan_schema(
                        &Routines,
                        source,
                        &[],
                        &context,
                        None,
                    )
                    .unwrap()
                })
                .unwrap_or_default();
            **block =
                crate::semantics::view_rewrite::resolve_outer_rewrites(block, &local).into_owned();
        }
        let mut inputs = 0;
        let mut visit = |expression: &ScalarExpr| {
            expression.visit(&mut |node| {
                if let ScalarExpr::InternalColumn(column) = node {
                    assert_eq!(column.relation(), raw);
                    assert!(column.attribute() >= source.len());
                    inputs += 1;
                }
            });
        };
        visit(&expression.scalar);
        for query in &expression.subqueries {
            query.visit_scalar_expressions(&mut visit);
        }
        assert_eq!(inputs, expected, "{sql}");
    }
}
