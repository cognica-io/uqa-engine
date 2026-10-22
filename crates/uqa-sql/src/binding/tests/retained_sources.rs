//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{Expr, FromClause, InternalRelationId, RetainedRowSource, Statement};
use crate::plan::UnifiedPlan;

fn source(rows: usize) -> RetainedRowSource {
    let relation = InternalRelationId::allocate();
    RetainedRowSource::new(
        relation,
        RowSchema::with_internal_relation_metadata(
            relation,
            [
                (Some(ColumnType::Integer), None),
                (Some(ColumnType::Text), Some(950)),
            ],
        ),
        rows,
    )
    .unwrap()
}

fn statement(source: &RetainedRowSource) -> Statement {
    let Statement::Select(mut query) = crate::compile("SELECT 1 AS n, 'x' AS text_value")
        .unwrap()
        .remove(0)
    else {
        panic!("SELECT")
    };
    query.from = Some(FromClause::RetainedRows(source.clone()));
    for (position, projection) in query.projections.iter_mut().enumerate() {
        projection.expr = Expr::InternalColumn(source.relation().column(position));
    }
    Statement::Select(query)
}

#[test]
fn retained_sources_bind_complete_types_without_sampling_or_exposing_names() {
    let context = crate::binding::fixture::empty_binding_context();
    for count in [0, 1, 1_000_000] {
        let source = source(count);
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(statement(&source)) else {
            panic!("query")
        };
        let RelationalPlan::QueryBlock(block) = &query.root else {
            panic!("block")
        };
        let Some(SourcePlan::RetainedRows(lowered)) = &block.from else {
            panic!("retained source")
        };
        assert_eq!(lowered.rows(), count);
        assert!(lowered.schema().shares_index(source.schema()));
        assert_eq!(block.from.as_ref().unwrap().visible_qualifier(), None);
        let bound = crate::binding::bind_query_plan_schema(
            &EmptyRoutineResolution,
            &query,
            &[],
            &context,
            None,
        )
        .unwrap();
        let analyzed = crate::binding::analyze_query_plan_schema(
            &EmptyRoutineResolution,
            &query,
            &[],
            &context,
            None,
        )
        .unwrap();
        assert_eq!(bound, analyzed);
        assert_eq!(
            bound.column_types(),
            &[Some(ColumnType::Integer), Some(ColumnType::Text)]
        );
        assert_eq!(bound.column_collation(1), Some(950));
        assert!(!crate::binding::correlation::query_depends_on_outer_row(
            crate::binding::correlation::CorrelationContext {
                routines: &super::EmptyRoutineResolution,
                catalog: context.catalog.as_ref(),
                resolution: &context.resolution,
                scoped_relations: None,
            },
            &query,
        )
        .unwrap());
    }
}

#[test]
fn retained_sources_cannot_escape_into_stored_syntax_or_sql_text() {
    let source = source(2);
    let statement = statement(&source);
    assert!(serde_json::to_value(&statement).is_err());
    assert!(crate::render::statement_sql(&statement).is_err());
    assert!(serde_json::to_value(UnifiedPlan::lower(statement)).is_err());
    assert!(serde_json::from_str::<FromClause>(r#"{"RetainedRows":{}}"#).is_err());
    assert!(serde_json::from_str::<SourcePlan>(r#"{"RetainedRows":{}}"#).is_err());
    assert!(
        RetainedRowSource::new(InternalRelationId::allocate(), source.schema().clone(), 2).is_err()
    );
    assert!(RetainedRowSource::new(
        source.relation(),
        RowSchema::with_types(vec!["visible".into()], vec![Some(ColumnType::Text)]),
        2,
    )
    .is_err());
    let aliased = RowSchema::with_physical_identity_aliases(
        source.schema(),
        &[(
            crate::schema::ColumnIdentity::unqualified("hidden_alias"),
            0,
            Some(ColumnType::Integer),
        )],
    );
    assert!(RetainedRowSource::new(source.relation(), aliased, 2).is_err());
}

#[test]
fn retained_sources_share_owned_record_descriptors_and_clone_without_row_values() {
    let relation = InternalRelationId::allocate();
    let fields: std::sync::Arc<[Option<ColumnType>]> =
        vec![Some(ColumnType::SmallInteger), Some(ColumnType::Text)].into();
    let visible = RowSchema::with_types(vec!["record".into()], vec![Some(ColumnType::Record)])
        .with_record_fields([(0, fields.clone())]);
    let internal = RowSchema::project_with_sources(
        &visible,
        vec![],
        vec![(
            relation.column(0),
            Some(ColumnType::Record),
            crate::schema::ProjectedSlot::Input(Some(0)),
        )],
        0,
        false,
    );
    let source = RetainedRowSource::new(relation, internal, 1_000_000).unwrap();
    let allocated = allocation_counter::measure(|| {
        let copies: Vec<_> = (0..128).map(|_| source.clone()).collect();
        for copy in copies {
            assert!(copy.schema().shares_index(source.schema()));
            assert!(std::sync::Arc::ptr_eq(
                copy.schema().physical_record_fields(0).unwrap(),
                &fields
            ));
        }
    });
    assert!(allocated.bytes_total < 32 * 1024, "{allocated:?}");
}
