//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn fixture(failure: Failure) -> Fixture {
    let mut fixture = Fixture::new(failure);
    fixture.rows = [
        ("first", "SELECT value FROM public.second", 1),
        ("second", "SELECT 'x'::text COLLATE \"C\" AS value", 2),
    ]
    .into_iter()
    .map(|(name, sql, identity)| {
        let mut stored = view(identity, RelationPersistence::Permanent);
        let UnifiedPlan::Query(query) =
            UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        stored.query = *query;
        stored.kind = StoredViewKind::Materialized;
        stored.materialized_column_types = vec![Some(ColumnType::Text)];
        stored.materialized_rows =
            vec![[("value".into(), uqa_core::Value::Str("retained".into()))].into()];
        catalog_view_row(&RelationIdentity::new("public", name), &stored).unwrap()
    })
    .collect();
    fixture
}

struct Bindings<'a>(&'a Fixture);
impl ViewPlanBinding for Bindings<'_> {
    fn lock_relations(&self, _: &QueryPlan) -> Result<(), SQLError> {
        panic!("restore does not execute")
    }
    fn bind_relations(&self, _: &mut QueryPlan) -> Result<bool, SQLError> {
        panic!("bound namespace")
    }
    fn bind_type_identities(&self, _: &mut QueryPlan) -> Result<(), SQLError> {
        panic!("retained identities")
    }
    fn assign_input_collations(&self, _: &mut QueryPlan) -> Result<(), SQLError> {
        // The fixture's definitions call no collation-dependent routine.
        Ok(())
    }
    fn bind_routines(&self, plan: &mut QueryPlan, _: &[SQLParam]) -> Result<RowSchema, SQLError> {
        self.0.assert_provisional_namespace();
        self.0.bindings.set(self.0.bindings.get() + 1);
        let mut dependencies = uqa_sql::catalog::events::RuleDependencies::default();
        uqa_sql::catalog::events::dependencies::collect_query_relation_dependencies(
            plan,
            &mut dependencies,
            &BTreeSet::new(),
        )?;
        for dependency in dependencies.relations {
            assert_eq!(dependency.name, "second");
            assert_eq!(
                self.0.bindings.get(),
                2,
                "dependent query follows its source"
            );
            assert_eq!(
                self.0.registry.borrow()[&dependency]
                    .materialized_schema()?
                    .column_collation(0),
                Some(950)
            );
        }
        if self.0.failure == Failure::Binding && self.0.bindings.get() == 2 {
            return Err(SQLError::Internal("injected binding failure".into()));
        }
        Ok(
            RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Text)])
                .with_column_collations([Some(950)]),
        )
    }
}

#[test]
fn materialized_collation_migration_orders_dependencies_preserves_rows_and_reopens_without_reanalysis(
) {
    let mut fixture = fixture(Failure::None);
    {
        let bindings = Bindings(&fixture);
        let mut context = fixture.context();
        context.bindings = &bindings;
        restore_views_from_catalog(&context, &fixture, true).unwrap();
    }
    assert_eq!(fixture.bindings.get(), 2);
    assert_eq!(fixture.identities.get(), 0);
    assert_eq!(fixture.saved.borrow().len(), 2);
    for stored in fixture
        .registry
        .borrow()
        .values()
        .filter(|view| view.kind == StoredViewKind::Materialized)
    {
        assert_eq!(stored.materialized_column_collations, Some(vec![950]));
        assert_eq!(
            stored.materialized_rows[0]["value"],
            uqa_core::Value::Str("retained".into())
        );
    }
    fixture.rows = fixture.saved.borrow().clone();
    fixture.saved.borrow_mut().clear();
    fixture.bindings.set(0);
    restore_views_from_catalog(&fixture.context(), &fixture, false).unwrap();
    assert_eq!(fixture.bindings.get(), 0);
    assert!(fixture.saved.borrow().is_empty());
}

#[test]
fn materialized_collation_migration_rejects_load_only_and_restores_registry_on_failure() {
    for failure in [Failure::None, Failure::Binding, Failure::Persistence] {
        let fixture = fixture(failure);
        let before = fixture.snapshot();
        let bindings = Bindings(&fixture);
        let mut context = fixture.context();
        context.bindings = &bindings;
        let error =
            restore_views_from_catalog(&context, &fixture, failure != Failure::None).unwrap_err();
        assert!(error.to_string().contains(if failure == Failure::None {
            "initial-open metadata migration"
        } else {
            "injected"
        }));
        assert_eq!(fixture.snapshot(), before);
        if failure == Failure::None {
            assert_eq!(fixture.writes.get(), 0);
            assert_eq!(fixture.bindings.get(), 0);
        }
    }
}
