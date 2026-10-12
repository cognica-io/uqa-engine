//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Projection preserves independent SQL collations while sharing original row payloads.

use super::*;
use crate::batch::{ColumnIdentity, PhysicalLayout};
use uqa_sql::ast::ColumnType;

#[test]
fn collation_projection_shares_payloads_and_preserves_each_bound_identity() {
    let schema = RowSchema::from_physical_layout(PhysicalLayout {
        columns: vec!["c".into(), "p".into()],
        identities: vec![
            ColumnIdentity::qualified("source", "c"),
            ColumnIdentity::qualified("source", "p"),
        ],
        types: vec![Some(ColumnType::Text); 2],
        slots: vec![Some(0); 2],
        physical_width: 1,
        aliases: vec![],
        internal: vec![],
        score_sources: vec![],
        wildcard_hidden: std::collections::HashSet::default(),
    })
    .unwrap()
    .with_column_collations([Some(950), Some(951)]);
    let schema = RowSchema::with_identity_aliases(
        &schema,
        &[
            (ColumnIdentity::qualified("aliases", "c"), 0),
            (ColumnIdentity::qualified("aliases", "p"), 1),
        ],
    );
    let original = crate::PhysicalRow::from_values(vec![Value::Str("x".repeat(4096))]);
    for expressions in [
        vec![("p", col("p")), ("c", col("c"))],
        vec![("ignored", ScalarExpr::Star)],
        vec![("ignored", ScalarExpr::QualifiedStar("source".into()))],
        vec![("ignored", ScalarExpr::QualifiedStar("aliases".into()))],
        vec![
            (
                "p",
                ScalarExpr::QualifiedColumn {
                    qualifier: "aliases".into(),
                    column: "p".into(),
                },
            ),
            (
                "c",
                ScalarExpr::QualifiedColumn {
                    qualifier: "aliases".into(),
                    column: "c".into(),
                },
            ),
        ],
    ] {
        let scan = TableScan::from_physical_rows(schema.clone(), vec![original.clone()]);
        let mut project = Project::new(
            Box::new(scan),
            expressions
                .into_iter()
                .map(|(name, expression)| (name.into(), expression))
                .collect(),
            vec![],
        );
        project.open().unwrap();
        let batch = project.next().unwrap().unwrap();
        project.close().unwrap();
        assert_eq!(batch.schema.collation_of("c"), Some(950));
        assert_eq!(batch.schema.collation_of("p"), Some(951));
        assert_eq!(batch.schema.physical_width(), 1);
        assert_eq!(batch.rows[0].fragment_count(), original.fragment_count());
        assert!(std::ptr::eq(
            batch.rows[0].value(0).unwrap(),
            original.value(0).unwrap()
        ));
    }
}

#[test]
fn computed_projection_keeps_selected_collations_for_empty_and_appended_results() {
    use crate::query::relational::values::test_support::Services;
    let mut plan = uqa_sql::plan::UnifiedPlan::lower(
        uqa_sql::compile("SELECT 'x'::text COLLATE \"C\" AS c, NULL::text COLLATE \"POSIX\" AS p, lower('ABC'::text COLLATE \"C\") AS folded")
            .unwrap().remove(0),
    );
    uqa_sql::binding::statements::analyze_plan_result(
        &Services,
        &Services,
        &mut plan,
        &[],
        &crate::catalog::test_support::empty_scope(),
    )
    .unwrap();
    let uqa_sql::plan::UnifiedPlan::Query(plan) = plan else {
        panic!("query")
    };
    let uqa_sql::plan::RelationalPlan::QueryBlock(block) = plan.root else {
        panic!("block")
    };
    let internal = InternalRelationId::allocate().column(0);
    for populated in [false, true] {
        for append in [false, true] {
            let schema = RowSchema::with_types(vec!["id".into()], vec![Some(ColumnType::Integer)]);
            let original = crate::PhysicalRow::from_values(vec![Value::Int(42)]);
            let rows = if populated {
                vec![original.clone()]
            } else {
                vec![]
            };
            let scan = Box::new(TableScan::from_physical_rows(schema, rows));
            let projections = block
                .projections
                .iter()
                .enumerate()
                .map(|(index, projection)| {
                    let target = if index == 2 {
                        ProjectionTarget::Internal(internal)
                    } else {
                        ProjectionTarget::Column(projection.alias.clone().unwrap())
                    };
                    (target, projection.expr.clone())
                })
                .collect();
            let mut project = if append {
                Project::appending_targets(scan, projections, vec![])
            } else {
                Project::with_targets(scan, projections, vec![])
            };
            let output = project.row_schema().clone();
            assert_eq!(output.collation_of("c"), Some(950));
            assert_eq!(output.collation_of("p"), Some(951));
            assert_eq!(output.internal_collation(internal), Some(950));
            project.open().unwrap();
            let batch = project.next().unwrap();
            project.close().unwrap();
            if populated {
                let batch = batch.unwrap();
                let view = batch.schema.view(&batch.rows[0]);
                assert_eq!(
                    view.value_at(output.position("c").unwrap()),
                    Some(&Value::Str("x".into()))
                );
                assert_eq!(
                    view.value_at(output.position("p").unwrap()),
                    Some(&Value::Null)
                );
                assert_eq!(
                    batch.rows[0].value(output.internal_slot(internal).unwrap()),
                    Some(&Value::Str("abc".into()))
                );
                assert!(std::ptr::eq(
                    batch.rows[0].value(0).unwrap(),
                    original.value(0).unwrap()
                ));
            } else {
                assert!(batch.is_none());
            }
        }
    }
}
