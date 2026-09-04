//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use uqa_execution::{FunctionTypeResolver, RowSchema};
use uqa_planner::{
    AccessPathPlan, ComputePlan, ProjectionPlan, QueryBlockPlan, QueryPlan, RelationalPlan,
    SourcePlan,
};
use uqa_sql::ast::{ColumnDef, ColumnType};
use uqa_sql::SQLError;

use super::analyze_query_plan_schema_with_catalog;
use crate::engine_capabilities::{CatalogReadView, CatalogTableSnapshot, RelationNameResolution};
use crate::engine_user_functions::RoutineResolution;
use crate::RelationIdentity;

struct EmptyRoutineResolution;

impl FunctionTypeResolver for EmptyRoutineResolution {
    fn resolve_function_type(
        &self,
        _name: &str,
        _binding: Option<&uqa_sql::ast::FunctionBinding>,
        _argument_names: &[Option<String>],
        _argument_types: &[Option<ColumnType>],
        _explicit_variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl RoutineResolution for EmptyRoutineResolution {}

fn column(name: &str, ty: ColumnType) -> ColumnDef {
    ColumnDef {
        name: name.into(),
        ty,
        object_id: None,
        missing_value: None,
        primary_key: false,
        not_null: false,
        not_null_explicit: false,
        not_null_name: None,
        not_null_validated: true,
        not_null_no_inherit: false,
        auto_increment: None,
        unique: false,
        default: None,
        generated: None,
        check: None,
        check_name: None,
        check_enforced: true,
        check_validated: true,
        check_no_inherit: false,
        references: None,
    }
}

#[test]
fn complete_query_binding_uses_catalog_fixture_without_engine() {
    let catalog = CatalogReadView::fixture(BTreeMap::from([
        (
            RelationIdentity::new("app", "documents"),
            CatalogTableSnapshot::fixture(vec![
                column("id", ColumnType::BigInteger),
                column("title", ColumnType::Text),
            ]),
        ),
        (
            RelationIdentity::new("app", "rankings"),
            CatalogTableSnapshot::fixture(vec![
                column("document_id", ColumnType::BigInteger),
                column("score", ColumnType::DoublePrecision),
            ]),
        ),
    ]));
    let resolution = RelationNameResolution::fixture(vec!["app".into()], "pg_temp_fixture".into());
    let source = SourcePlan::Join {
        left: Box::new(SourcePlan::Table {
            name: "documents".into(),
            qualifier: "documents".into(),
            alias: Some("d".into()),
            column_aliases: Vec::new(),
            include_descendants: true,
        }),
        right: Box::new(SourcePlan::Table {
            name: "rankings".into(),
            qualifier: "rankings".into(),
            alias: Some("r".into()),
            column_aliases: Vec::new(),
            include_descendants: true,
        }),
        kind: uqa_sql::ast::JoinKind::Inner,
        on: Some(uqa_execution::ScalarExpr::Binary {
            op: uqa_sql::ast::BinaryOp::Equal,
            lhs: Box::new(uqa_execution::ScalarExpr::qualified_column("d", "id")),
            rhs: Box::new(uqa_execution::ScalarExpr::qualified_column(
                "r",
                "document_id",
            )),
        }),
        using: None,
        natural: false,
        alias: None,
        column_aliases: Vec::new(),
        lateral: false,
        strategy: uqa_planner::JoinExecutionStrategy::Hash,
    };
    let plan = QueryPlan {
        relations_bound: false,
        ctes: Vec::new(),
        root: RelationalPlan::QueryBlock(Box::new(QueryBlockPlan {
            projections: vec![
                ProjectionPlan {
                    expr: uqa_execution::ScalarExpr::qualified_column("d", "title"),
                    alias: Some("title".into()),
                },
                ProjectionPlan {
                    expr: uqa_execution::ScalarExpr::qualified_column("r", "score"),
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
    };

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
