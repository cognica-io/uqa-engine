//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::query::relational::{values::test_support::Services, QueryExpressionFactory};
use crate::{Batch, OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr, SpillBuffer};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use uqa_core::Value;

#[derive(Default)]
struct PreparationCounter(AtomicUsize);

impl QueryExpressionFactory<()> for PreparationCounter {
    fn production_resources(&self) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
        Ok(uqa_core::memory::ProductionControl::uncontrolled().capture())
    }
    fn bind_scope(
        &self,
        _: CteScope,
    ) -> Arc<dyn crate::query::set_projection::SetFunctionRuntime + '_> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Arc::new(Services)
    }

    fn optimize_catalog_scalar(&self, _: &mut ScalarExpr) -> Result<(), SQLError> {
        panic!("mutation qualification must not add constant evaluation")
    }

    fn prepare_predicate<'a>(
        &'a self,
        _: &ScalarExpr,
        _: &'a [SQLParam],
        _: &CteScope,
    ) -> Result<Option<crate::SharedRowPredicate<'a>>, SQLError> {
        panic!("mutation qualification uses its statement read scope")
    }
}

fn update(sql: &str) -> UpdatePlan {
    let uqa_sql::plan::UnifiedPlan::Command(command) =
        uqa_sql::plan::UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("command");
    };
    let uqa_sql::plan::CommandPlan::Update(update) = *command else {
        panic!("update");
    };
    *update
}

fn schema(name: &str) -> RowSchema {
    RowSchema::with_types(vec![name.into()], vec![Some(uqa_sql::ColumnType::Integer)])
}

#[test]
fn update_source_qualification_prepares_once_for_fixed_statement_rows() {
    for budget in [1 << 20, 1] {
        let factory = PreparationCounter::default();
        let expressions = crate::mutation::rows::context::MutationExpressionContext {
            types: &Services,
            expressions: &factory,
        };
        let stmt =
            update("UPDATE target SET target_id=source_id FROM source WHERE target_id=source_id");
        let scope = CteScope::default();
        let mut qualification =
            MutationPredicate::new(expressions, &scope, stmt.predicate.as_ref(), &[]);
        let source_schema = schema("source_id");
        let mut spill = SpillBuffer::new(budget);
        spill
            .push(Batch::from_physical_rows(
                source_schema.clone(),
                (0..100)
                    .map(|id| PhysicalRow::from_values(vec![Value::Int(id)]))
                    .collect(),
            ))
            .unwrap();
        let sources = spill.into_shared(source_schema).unwrap();
        assert_eq!(sources.has_spilled(), budget == 1);
        for id in 0..100 {
            let target = OwnedPhysicalRow::new(
                schema("target_id"),
                PhysicalRow::from_values(vec![Value::Int(id)]),
            );
            let matched = sources
                .read_rows()
                .unwrap()
                .filter_map(|source| {
                    let source = source.unwrap();
                    update_join_qualifies(&mut qualification, &target, &source)
                        .unwrap()
                        .then_some(source)
                })
                .collect::<Vec<_>>();
            assert_eq!(matched.len(), 1);
            assert_eq!(matched[0].row.value(0), Some(&Value::Int(id)));
        }
        assert_eq!(
            factory.0.load(Ordering::Relaxed),
            1,
            "the same UPDATE predicate was rebound for each target/source pair"
        );
    }
}

#[test]
fn prepared_update_qualification_keeps_lazy_errors_nulls_and_schema_changes() {
    let factory = PreparationCounter::default();
    let expressions = crate::mutation::rows::context::MutationExpressionContext {
        types: &Services,
        expressions: &factory,
    };
    let scope = CteScope::default();
    let stmt = update(
        "UPDATE target SET target_id=source_id FROM source WHERE source_id=1 OR 10/(source_id-2)>0",
    );
    let mut qualification =
        MutationPredicate::new(expressions, &scope, stmt.predicate.as_ref(), &[]);
    assert_eq!(factory.0.load(Ordering::Relaxed), 0);
    let row =
        |value| OwnedPhysicalRow::new(schema("source_id"), PhysicalRow::from_values(vec![value]));
    assert!(qualification.qualifies(&row(Value::Int(1))).unwrap());
    assert!(!qualification.qualifies(&row(Value::Null)).unwrap());
    assert!(qualification.qualifies(&row(Value::Int(3))).unwrap());
    let error = qualification.qualifies(&row(Value::Int(2))).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22012"));
    assert_eq!(factory.0.load(Ordering::Relaxed), 1);

    let stmt = update("UPDATE target SET target_id=source_id FROM source WHERE source_id/2=1");
    let mut qualification =
        MutationPredicate::new(expressions, &scope, stmt.predicate.as_ref(), &[]);
    assert!(qualification.qualifies(&row(Value::Int(3))).unwrap());
    let floating = OwnedPhysicalRow::new(
        RowSchema::with_types(
            vec!["source_id".into()],
            vec![Some(uqa_sql::ColumnType::DoublePrecision)],
        ),
        PhysicalRow::from_values(vec![Value::Float(3.0)]),
    );
    assert!(!qualification.qualifies(&floating).unwrap());
    assert!(qualification.qualifies(&row(Value::Int(3))).unwrap());
    assert_eq!(factory.0.load(Ordering::Relaxed), 4);
}

#[test]
fn prepared_update_qualification_keeps_source_counts_order_and_rechecks() {
    let factory = PreparationCounter::default();
    let expressions = crate::mutation::rows::context::MutationExpressionContext {
        types: &Services,
        expressions: &factory,
    };
    let scope = CteScope::default();
    let stmt = update("UPDATE target SET target_id=source_id FROM source WHERE source_id>0");
    let mut qualification =
        MutationPredicate::new(expressions, &scope, stmt.predicate.as_ref(), &[]);
    let source_schema = schema("source_id");
    let target = OwnedPhysicalRow::new(
        schema("target_id"),
        PhysicalRow::from_values(vec![Value::Int(7)]),
    );
    let empty = SpillBuffer::new(1)
        .into_shared(source_schema.clone())
        .unwrap();
    assert_eq!(empty.read_rows().unwrap().count(), 0);
    assert_eq!(factory.0.load(Ordering::Relaxed), 0);
    let mut spill = SpillBuffer::new(1);
    spill
        .push(Batch::from_physical_rows(
            source_schema.clone(),
            [2, 1, 2, 0]
                .into_iter()
                .map(|value| PhysicalRow::from_values(vec![Value::Int(value)]))
                .collect(),
        ))
        .unwrap();
    let sources = spill.into_shared(source_schema).unwrap();
    let matched = sources
        .read_rows()
        .unwrap()
        .filter_map(|source| {
            let source = source.unwrap();
            update_join_qualifies(&mut qualification, &target, &source)
                .unwrap()
                .then_some(source)
        })
        .collect::<Vec<_>>();
    assert_eq!(matched.len(), 3);
    let first = &matched[0];
    assert_eq!(first.row.value(0), Some(&Value::Int(2)));
    assert!(update_join_qualifies(&mut qualification, &target, first).unwrap());
    let changed = OwnedPhysicalRow::new(
        schema("source_id"),
        PhysicalRow::from_values(vec![Value::Int(0)]),
    );
    assert!(!update_join_qualifies(&mut qualification, &target, &changed).unwrap());
    assert_eq!(factory.0.load(Ordering::Relaxed), 1);
}
