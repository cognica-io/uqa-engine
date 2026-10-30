//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::events::selection::CapturedRules,
    plan::{InsertPlan, RelationalPlan, ViewRuleInsertPlan},
    semantics::rules::binding::OwnedInsertRuleQueries,
};
use std::{cell::Cell, sync::Arc};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, RelationIdentity, Value,
};

#[derive(Default)]
struct Rules(Arc<crate::catalog::events::RuleCatalog>);
impl RuleCatalog for Rules {
    fn relation_has_rules(&self, table: &str) -> Result<bool, SQLError> {
        Ok(self.0.contains_key(&self.resolve_rule_relation(table)?))
    }
    fn resolve_rule_relation(&self, table: &str) -> Result<RelationIdentity, SQLError> {
        RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)
    }
    fn rules_for(&self, table: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        Ok(crate::catalog::events::selection::active_rules(
            &self.0,
            &self.resolve_rule_relation(table)?,
            event,
            false,
        ))
    }
    fn resolve_mutation_target(&self, name: &str, _: bool) -> Result<String, SQLError> {
        Ok(name.into())
    }
    fn capture_rules_with_control(
        &self,
        table: &str,
        event: RuleEvent,
        control: &ProductionControl<'_>,
    ) -> Result<CapturedRules, SQLError> {
        CapturedRules::from_catalog(
            Arc::clone(&self.0),
            self.resolve_rule_relation_with_control(table, control)?,
            event,
            false,
            control,
        )
    }
}
impl Rules {
    fn new(statements: &[&str]) -> Self {
        let mut rules = crate::catalog::events::RuleCatalog::new();
        for sql in statements {
            let Statement::CreateRule(definition) = crate::compile(sql).unwrap().remove(0) else {
                panic!("rule")
            };
            let table = RelationIdentity::from_legacy_name(&definition.table).unwrap();
            Arc::make_mut(rules.entry(table).or_default()).insert(
                definition.name.clone(),
                StoredRule {
                    definition,
                    enabled: crate::ast::EventEnableMode::default(),
                    condition_plan: None,
                    condition_binding: None,
                    dependencies: None,
                    catalog_oid: None,
                },
            );
        }
        Self(Arc::new(rules))
    }
}
fn insert(sql: &str) -> Box<InsertPlan> {
    let CommandPlan::Insert(insert) = *command(sql) else {
        panic!("INSERT")
    };
    insert
}
fn layered(sql: &str) -> Box<InsertPlan> {
    let mut insert = insert(sql);
    insert.view_rule_relations = vec!["outer_view".into(), "inner_view".into()];
    insert.view_rule_insert_plans = ["outer_view", "inner_view"]
        .into_iter()
        .map(|relation| ViewRuleInsertPlan {
            relation: relation.into(),
            supplied_columns: insert
                .columns
                .iter()
                .map(|column| column.column.clone())
                .collect(),
            input_columns: vec!["a".into(), "b".into()],
        })
        .collect();
    insert
}
fn normalize_original(original: Option<&QueryPlan>) -> serde_json::Value {
    super::owned::normalize_value(&original)
}

#[test]
fn admitted_insert_schedules_preserve_view_defaults_and_original_qualifications() {
    let catalog = super::owned::catalog_fixture();
    let rule_sets = [
        Rules::default(),
        Rules::new(&["CREATE RULE skip_outer AS ON INSERT TO outer_view WHERE NEW.a > 9 DO INSTEAD NOTHING", "CREATE RULE skip_inner AS ON INSERT TO inner_view WHERE NEW.a = (SELECT 13) DO INSTEAD NOTHING", "CREATE RULE skip_base AS ON INSERT TO items WHERE NEW.a < 0 DO INSTEAD NOTHING"]),
        Rules::new(&["CREATE RULE suppress AS ON INSERT TO inner_view DO INSTEAD NOTHING"]),
    ];
    for sql in [
        "INSERT INTO items(a,b) VALUES (DEFAULT, DEFAULT)",
        "INSERT INTO items(a,b) VALUES (1, ARRAY[2]), (DEFAULT, DEFAULT)",
        "INSERT INTO items(a) VALUES (DEFAULT), (3)",
        "INSERT INTO items(b[1]) VALUES (7)",
        "INSERT INTO items DEFAULT VALUES",
        "INSERT INTO items(a,b) VALUES ((SELECT 1), ARRAY[(SELECT 2)])",
        "INSERT INTO items(a,b) SELECT 1, ARRAY[2] UNION ALL SELECT 3, ARRAY[4]",
    ] {
        for rules in &rule_sets {
            let input = layered(sql);
            let context = RuleAnalysisContext {
                rules,
                ..catalog.analysis()
            };
            let expected =
                InsertRuleQueries::bind(context, &input, &|_| Ok("owner".into()), &"caller".into())
                    .unwrap();
            let budget = MemoryBudget::new(16 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let mut result = None;
            let observed = allocation_counter::measure(|| {
                result = Some(
                    OwnedInsertRuleQueries::bind(
                        context,
                        &input,
                        &|_| Ok("owner".into()),
                        &"caller".into(),
                        &control,
                    )
                    .unwrap(),
                );
            });
            let result = result.unwrap();
            assert_eq!(
                observed.bytes_current as usize,
                budget.used(),
                "{sql}: {observed:?}"
            );
            assert_eq!(result.reserved_bytes(), budget.used());
            assert_eq!(
                normalize_original(result.original.as_deref()),
                normalize_original(expected.original.as_deref()),
                "{sql}"
            );
            let actual = result
                .relations()
                .map(super::owned::normalize_value)
                .collect::<Vec<_>>();
            let expected = expected
                .relations
                .iter()
                .map(super::owned::normalize_value)
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{sql}");
            drop(result);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn insert_layer_selection_discards_large_replaced_inputs_before_copying() {
    let mut input = layered("INSERT INTO items(a) SELECT 1");
    let RelationalPlan::QueryBlock(block) = &mut input.source.as_mut().unwrap().root else {
        panic!("query")
    };
    block.projections[0].expr = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    input.columns[0].column = "replaced".repeat(1 << 20);
    input.view_rule_relations.push("unused".repeat(1 << 20));
    input.view_rule_insert_plans[1]
        .input_columns
        .push("unused".repeat(1 << 20));
    let budget = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let mut selected = None;
    let observed = allocation_counter::measure(|| {
        selected = Some(
            input
                .copy_rule_layer_for_rewrite(input.view_rule_insert_plans.first(), false, &control)
                .unwrap(),
        );
    });
    let selected = selected.unwrap();
    assert_eq!(observed.bytes_current as usize, budget.used());
    assert!(observed.bytes_total < 64 << 10, "{observed:?}");
    let CommandPlan::Insert(plan) = &*selected else {
        panic!("INSERT")
    };
    assert!(plan.source.is_none());
    assert!(plan.view_rule_relations.is_empty());
    assert!(plan.view_rule_insert_plans.is_empty());
    assert_eq!(plan.table, "outer_view");
    assert_eq!(plan.columns[0].column, "a");
    drop(selected);
    assert_eq!(budget.used(), 0);
}

#[test]
fn insert_scheduling_rejects_original_payloads_and_preserves_live_siblings() {
    let catalog = super::owned::catalog_fixture();
    let mut input = layered("INSERT INTO items(a) VALUES (1)");
    input.rows[0][0] = ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
    for limit in [0, 512, 8192, 65536] {
        let budget = MemoryBudget::new(limit + 8);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let sibling = control.copy_text("retained").unwrap();
        let calls = Cell::new(0);
        let observed = allocation_counter::measure(|| {
            let error = OwnedInsertRuleQueries::bind(
                catalog.analysis(),
                &input,
                &|_| {
                    calls.set(calls.get() + 1);
                    Ok("owner".into())
                },
                &"caller".into(),
                &control,
            )
            .err()
            .unwrap();
            assert_eq!(error.sqlstate(), Some("53200"));
        });
        assert!(observed.bytes_total < 128 << 10, "{observed:?}");
        assert_eq!(calls.get(), 0);
        assert_eq!(budget.used(), sibling.reserved_bytes());
        drop(sibling);
        assert_eq!(budget.used(), 0);
    }
}

#[test]
fn insert_schedule_cancellation_and_subject_failure_release_only_their_owners() {
    let catalog = super::owned::catalog_fixture();
    let rules = Rules::new(&[
        "CREATE RULE skip AS ON INSERT TO outer_view WHERE NEW.a > 0 DO INSTEAD NOTHING",
    ]);
    let context = RuleAnalysisContext {
        rules: &rules,
        ..catalog.analysis()
    };
    let input = layered("INSERT INTO items(a) VALUES (1)");
    for original_cancelled in [false, true] {
        let budget = MemoryBudget::new(4 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let sibling = control.copy_text("retained").unwrap();
        if original_cancelled {
            original.cancel();
        } else {
            invoking.cancel();
        }
        let error = OwnedInsertRuleQueries::bind(
            context,
            &input,
            &|_| panic!("cancelled before subject lookup"),
            &"caller".into(),
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(budget.used(), sibling.reserved_bytes());
    }
    let budget = MemoryBudget::new(4 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let sibling = control.copy_text("retained").unwrap();
    let error = OwnedInsertRuleQueries::bind(
        context,
        &input,
        &|_| Err(SQLError::UnknownTable("missing".into())),
        &"caller".into(),
        &control,
    )
    .err()
    .unwrap();
    assert_eq!(error.sqlstate(), Some("42P01"));
    assert_eq!(budget.used(), sibling.reserved_bytes());
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = OwnedInsertRuleQueries::bind(
            context,
            &input,
            &|_| panic!("subject lookup"),
            &"caller".into(),
            &control,
        );
    }));
    assert!(unwind.is_err());
    assert_eq!(budget.used(), sibling.reserved_bytes());
    let result = OwnedInsertRuleQueries::bind(
        context,
        &input,
        &|_| Ok("owner".into()),
        &"caller".into(),
        &control,
    )
    .unwrap();
    drop(result);
    assert_eq!(budget.used(), sibling.reserved_bytes());
    drop(sibling);
    assert_eq!(budget.used(), 0);
}

mod row_columns;
