//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::ast::{ColumnDef, ColumnType};
use crate::plan::{
    AccessPathPlan, ComputePlan, ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan,
    SourcePlan,
};
use crate::SQLError;
use crate::{FunctionTypeResolver, RowSchema};

use super::analyze_query_plan_schema_with_catalog;
use crate::routines::RoutineResolution;
use crate::RelationIdentity;

mod collations;
mod empty_relations;
mod repeated_query_binding;
mod retained_sources;

struct EmptyRoutineResolution;

impl FunctionTypeResolver for EmptyRoutineResolution {
    fn resolve_function_type(
        &self,
        _name: &str,
        _binding: Option<&crate::ast::FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl RoutineResolution for EmptyRoutineResolution {}

#[test]
fn retrieval_metadata_binding_agrees_with_analysis_without_running_a_query() {
    let context = super::BindingContext {
        catalog: super::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "messages"),
            super::fixture::table_definition(vec![
                column("id", ColumnType::Integer),
                column("content", ColumnType::Text),
                column("embedding", ColumnType::Vector(2)),
            ]),
        )])),
        ..super::fixture::empty_binding_context()
    };
    for sql in [
        "SELECT id, _score FROM messages WHERE text_match(content, 'learning')",
        "SELECT id, _score FROM messages WHERE pool_positive_evidence(\
            bayesian_match(content, 'learning'), knn_match(embedding, ARRAY[0.9, 0.1], 3))",
        "SELECT id, _score FROM (SELECT id, _score FROM messages WHERE \
            pool_positive_evidence(bayesian_match(content, 'learning'), \
            knn_match(embedding, ARRAY[0.9, 0.1], 3))) hits",
        "WITH hits AS (SELECT id, _score FROM messages WHERE \
            pool_positive_evidence(bayesian_match(content, 'learning'), \
            knn_match(embedding, ARRAY[0.9, 0.1], 3))) SELECT id, _score FROM hits",
    ] {
        let crate::plan::UnifiedPlan::Query(query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("SELECT");
        };
        let bound =
            super::bind_query_plan_schema(&EmptyRoutineResolution, &query, &[], &context, None)
                .unwrap();
        let analyzed =
            super::analyze_query_plan_schema(&EmptyRoutineResolution, &query, &[], &context, None)
                .unwrap();
        assert_eq!(
            bound.column_types(),
            &[Some(ColumnType::Integer), Some(ColumnType::DoublePrecision)],
            "{sql}",
        );
        assert_eq!(bound, analyzed, "{sql}");
    }
}

#[test]
fn deferred_recursive_ctes_keep_the_complete_query_descriptor() {
    for sql in [
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<3) SELECT n FROM r",
        "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM r WHERE n<3) SEARCH DEPTH FIRST BY n SET ordering SELECT n,ordering FROM r",
        "WITH RECURSIVE seed(n) AS (SELECT 1), r(n) AS (SELECT n FROM seed UNION ALL SELECT n+1 FROM r WHERE n<3) CYCLE n SET cycled USING path SELECT n,cycled,path FROM r",
    ] {
        let crate::plan::UnifiedPlan::Query(mut query) =
            crate::plan::UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("expected query plan");
        };
        let mut context = super::fixture::empty_binding_context();
        let expected =
            super::bind_query_plan_schema(&EmptyRoutineResolution, &query, &[], &context, None)
                .unwrap();
        for cte in std::mem::take(&mut query.ctes) {
            context.deferred_ctes.insert(cte.name.clone(), cte.into());
        }
        for _ in 0..2 {
            let actual =
                super::bind_query_plan_schema(&EmptyRoutineResolution, &query, &[], &context, None)
                    .unwrap();
            assert_eq!(actual, expected, "{sql}");
        }
    }
}

#[test]
fn routine_parameter_lookup_preserves_wildcard_syntax_sites() {
    let context = super::BindingContext {
        catalog: super::fixture::catalog(BTreeMap::new()),
        resolution: super::fixture::resolution(vec!["public".into()], "pg_temp_fixture".into()),
        ctes: BTreeMap::new(),
        deferred_ctes: BTreeMap::new(),
        non_returning_ctes: std::collections::BTreeSet::new(),
        scalar_subqueries: &[],
    };
    let params = [crate::SQLParam::typed_scalar(
        uqa_core::Value::Null,
        ColumnType::Text,
    )];
    let parameters =
        super::RoutineParameterScope::new("f", vec!["input".into()], vec![Some(ColumnType::Text)]);
    for star in ["*", "src.*"] {
        let statement = crate::compile(&format!(
            "SELECT {star}, input FROM (VALUES (1, 2)) AS src(a, b)"
        ))
        .unwrap()
        .remove(0);
        let mut plan = crate::plan::UnifiedPlan::lower(statement);
        let original = plan.clone();
        super::bind_routine_parameter_references(
            &EmptyRoutineResolution,
            &mut plan,
            &params,
            &context,
            &parameters,
        )
        .unwrap();
        let crate::plan::UnifiedPlan::Query(query) = plan else {
            panic!("SELECT")
        };
        let crate::plan::UnifiedPlan::Query(original) = original else {
            panic!("SELECT")
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("query block")
        };
        let RelationalPlan::QueryBlock(original) = original.root else {
            panic!("query block")
        };
        assert_eq!(block.projections.len(), 2, "{star}");
        assert_eq!(block.projections[0].expr, original.projections[0].expr);
        assert_eq!(block.projections[0].alias, original.projections[0].alias);
        assert_eq!(block.projections[1].expr, crate::ScalarExpr::Param(1));
    }
    for grouping in ["GROUP BY input", "GROUP BY GROUPING SETS (input)"] {
        let statement = crate::compile(&format!(
            "SELECT a AS input FROM (VALUES (1, 2)) AS src(a, b) {grouping}"
        ))
        .unwrap()
        .remove(0);
        let mut plan = crate::plan::UnifiedPlan::lower(statement);
        super::bind_routine_parameter_references(
            &EmptyRoutineResolution,
            &mut plan,
            &params,
            &context,
            &parameters,
        )
        .unwrap();
        let crate::plan::UnifiedPlan::Query(query) = plan else {
            panic!("SELECT")
        };
        let RelationalPlan::QueryBlock(block) = query.root else {
            panic!("query block")
        };
        assert!(block
            .group_by
            .iter()
            .chain(block.grouping_sets.iter().flatten())
            .all(|item| matches!(item, crate::ScalarExpr::Column(name) if name == "input")));
    }
}

#[test]
fn result_schema_keeps_correlated_columns_without_reference_validation() {
    let context = super::BindingContext {
        catalog: super::fixture::catalog(BTreeMap::new()),
        resolution: super::fixture::resolution(vec!["public".into()], "pg_temp_fixture".into()),
        ctes: BTreeMap::new(),
        deferred_ctes: BTreeMap::new(),
        non_returning_ctes: std::collections::BTreeSet::new(),
        scalar_subqueries: &[],
    };
    for expression in [
        "(SELECT src.c)",
        "(SELECT (SELECT src.c))",
        "(SELECT src.c) + 1",
        "CASE WHEN true THEN (SELECT src.c) ELSE 0 END",
        "(SELECT src.c FROM (VALUES (2::integer)) AS src(c))",
    ] {
        let statement = crate::compile(&format!(
            "SELECT {expression} FROM (VALUES (1::integer)) AS src(c)"
        ))
        .unwrap()
        .remove(0);
        let crate::plan::UnifiedPlan::Query(query) = crate::plan::UnifiedPlan::lower(statement)
        else {
            panic!("SELECT")
        };
        let schema =
            super::bind_query_plan_schema(&EmptyRoutineResolution, &query, &[], &context, None)
                .unwrap();
        assert_eq!(
            schema.column_type(0),
            Some(&ColumnType::Integer),
            "{expression}"
        );
        let analyzed =
            super::analyze_query_plan_schema(&EmptyRoutineResolution, &query, &[], &context, None)
                .unwrap();
        assert_eq!(
            schema.column_types(),
            analyzed.column_types(),
            "{expression}"
        );
    }
}

fn column(name: &str, ty: ColumnType) -> ColumnDef {
    ColumnDef {
        collation_oid: None,
        name: name.into(),
        ty,
        object_id: None,
        attribute_number: None,
        missing_value: None,
        primary_key: false,
        not_null: false,
        not_null_explicit: false,
        not_null_name: None,
        not_null_identity: None,
        not_null_validated: true,
        not_null_no_inherit: false,
        not_null_is_local: true,
        auto_increment: None,
        unique: false,
        default: None,
        generated: None,
        check: None,
        check_name: None,
        check_enforced: true,
        check_validated: true,
        check_no_inherit: false,
        check_is_local: true,
        check_object_id: None,
        check_catalog_oid: None,
        default_catalog_oid: None,
        references: None,
    }
}

fn joined_catalog_query() -> QueryPlan {
    let source = SourcePlan::Join {
        comma: false,
        left: Box::new(SourcePlan::Table {
            executable_query: None,
            bound_columns: None,
            name: "documents".into(),
            qualifier: "documents".into(),
            alias: Some("d".into()),
            column_aliases: Vec::new(),
            include_descendants: true,
        }),
        right: Box::new(SourcePlan::Table {
            executable_query: None,
            bound_columns: None,
            name: "rankings".into(),
            qualifier: "rankings".into(),
            alias: Some("r".into()),
            column_aliases: Vec::new(),
            include_descendants: true,
        }),
        kind: crate::ast::JoinKind::Inner,
        on: Some(crate::ScalarExpr::Binary {
            op: crate::ast::BinaryOp::Equal,
            lhs: Box::new(crate::ScalarExpr::qualified_column("d", "id")),
            rhs: Box::new(crate::ScalarExpr::qualified_column("r", "document_id")),
        }),
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral: false,
        strategy: crate::plan::JoinExecutionStrategy::Hash,
    };
    QueryPlan {
        analyzed_output: None,
        analyzed_source: None,
        analyzed_grouping: None,
        relation_privilege_subject: None,
        relations_bound: false,
        ctes: Vec::new(),
        root: RelationalPlan::QueryBlock(Box::new(QueryBlockPlan {
            deferred_outer_rewrites: Vec::new(),
            privilege_columns: uqa_core::memory::OwnedSet::new(),
            windows: Vec::new(),
            projections: vec![
                ProjectionPlan {
                    expr: crate::ScalarExpr::qualified_column("d", "title"),
                    alias: Some("title".into()),
                },
                ProjectionPlan {
                    expr: crate::ScalarExpr::qualified_column("r", "score"),
                    alias: Some("score".into()),
                },
            ],
            from: Some(source),
            r#where: None,
            compute: ComputePlan::Project,
            group_by: Vec::new(),
            grouping_sets: Vec::new(),
            group_distinct: false,
            having: None,
            order_by: Vec::new(),
            limit: None,
            with_ties: false,
            offset: None,
            distinct: false,
            distinct_on: Vec::new(),
            subqueries: Vec::new(),
            access: AccessPathPlan::Row,
            locking: Vec::new(),
        })),
    }
}

#[test]
fn complete_query_binding_uses_catalog_fixture_without_engine() {
    let catalog = crate::binding::fixture::catalog(BTreeMap::from([
        (
            RelationIdentity::new("app", "documents"),
            crate::binding::fixture::table_definition(vec![
                column("id", ColumnType::BigInteger),
                column("title", ColumnType::Text),
            ]),
        ),
        (
            RelationIdentity::new("app", "rankings"),
            crate::binding::fixture::table_definition(vec![
                column("document_id", ColumnType::BigInteger),
                column("score", ColumnType::DoublePrecision),
            ]),
        ),
    ]));
    let resolution =
        crate::binding::fixture::resolution(vec!["app".into()], "pg_temp_fixture".into());
    let plan = joined_catalog_query();

    let schema = analyze_query_plan_schema_with_catalog(
        &EmptyRoutineResolution,
        &plan,
        &[],
        catalog,
        resolution,
    )
    .unwrap();

    assert_eq!(
        schema,
        RowSchema::with_types(
            vec!["title".into(), "score".into()],
            vec![Some(ColumnType::Text), Some(ColumnType::DoublePrecision)],
        )
    );
}

#[test]
fn deferred_cte_shadowing_uses_the_replacement_row_type() {
    use crate::binding::snapshot::BindingSnapshot;
    use std::collections::BTreeSet;
    for previously_non_returning in [false, true] {
        let statement =
            crate::compile("WITH source AS (SELECT true AS value) SELECT value FROM source")
                .unwrap()
                .remove(0);
        let crate::plan::UnifiedPlan::Query(mut plan) = crate::plan::UnifiedPlan::lower(statement)
        else {
            panic!("expected query plan");
        };
        let mut scope = BindingSnapshot {
            catalog: super::fixture::catalog(BTreeMap::new()),
            resolution: super::fixture::resolution(vec!["public".into()], "pg_temp_fixture".into()),
            ctes: BTreeMap::from([(
                "source".into(),
                RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Text)]),
            )]),
            deferred_ctes: BTreeMap::new(),
            non_returning_ctes: if previously_non_returning {
                BTreeSet::from(["source".into()])
            } else {
                BTreeSet::new()
            },
            scalar_subqueries: Arc::default(),
        };
        scope.insert_deferred(plan.ctes.remove(0));
        let schema = super::analyze_query_plan_schema(
            &EmptyRoutineResolution,
            &plan,
            &[],
            &scope.context(),
            None,
        )
        .unwrap();
        assert_eq!(schema.column_type(0), Some(&ColumnType::Boolean));
    }
}
