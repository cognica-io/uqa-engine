//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::plan::{ExpressionPlan, MutableExecutablePlan, RuleValuesInput};
use crate::semantics::rules::binding::OwnedRuleQueryInput;
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken, Value,
};

fn expression(sql: &str) -> Expr {
    let Statement::Select(mut query) = crate::compile(&format!("SELECT {sql}")).unwrap().remove(0)
    else {
        panic!("expression")
    };
    query.projections.remove(0).expr
}

pub(super) fn catalog_fixture() -> Catalog {
    let mut a = ColumnDef::nullable("a", ColumnType::Integer);
    a.default = Some(expression("(SELECT 17)"));
    let mut b = ColumnDef::nullable("b", ColumnType::Array(Box::new(ColumnType::Integer)));
    b.default = Some(expression("ARRAY[23]"));
    Catalog(vec![a, b])
}

pub(super) fn normalize(plan: &UnifiedPlan) -> serde_json::Value {
    normalize_value(plan)
}

pub(super) fn normalize_value(plan: &impl serde::Serialize) -> serde_json::Value {
    fn visit(value: &mut serde_json::Value, identities: &mut BTreeMap<u64, u64>) {
        match value {
            serde_json::Value::Object(fields) => {
                for (key, value) in fields {
                    if matches!(key.as_str(), "internal_relation" | "relation") {
                        if let Some(id) = value.as_u64() {
                            let next = identities.len() as u64;
                            *value = (*identities.entry(id).or_insert(next)).into();
                        }
                    }
                    visit(value, identities);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, identities);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(plan).unwrap();
    visit(&mut value, &mut BTreeMap::new());
    value
}

#[test]
fn native_values_actions_preserve_defaults_partial_targets_and_scalar_query_order() {
    let catalog = catalog_fixture();
    for sql in [
        "INSERT INTO log VALUES (DEFAULT, DEFAULT)",
        "INSERT INTO log (a, b) VALUES (1, ARRAY[2]), (DEFAULT, DEFAULT)",
        "INSERT INTO log (a) VALUES (DEFAULT), (4)",
        "INSERT INTO log (b[1]) VALUES (7)",
        "INSERT INTO log DEFAULT VALUES",
        "INSERT INTO log (a, b) VALUES ((SELECT 1), ARRAY[(SELECT 2)])",
    ] {
        let original = command(sql);
        let mut expected = original.clone();
        let CommandPlan::Insert(insert) = expected.as_mut() else {
            panic!("INSERT")
        };
        let mut values = RuleValuesInput::describe(insert).unwrap().unwrap();
        insert.source = Some(values.source(catalog.analysis(), insert).unwrap());
        values.project_layer(catalog.analysis(), insert).unwrap();
        let budget = MemoryBudget::new(4 << 20);
        let token = CancellationToken::new();
        let control = ProductionControl::new(&budget, &token, &token);
        let original = original.copy_for_rewrite(&control).unwrap();
        let before = budget.used();
        let mut result = None;
        let measured = allocation_counter::measure(|| {
            result = Some(
                RuleValuesInput::prepare_action_with_control(
                    catalog.analysis(),
                    original,
                    &control,
                )
                .unwrap(),
            );
        });
        let result = result.unwrap();
        assert_eq!(
            before as i64 + measured.bytes_current,
            budget.used() as i64,
            "{sql}"
        );
        assert_eq!(result.reserved_bytes(), budget.used(), "{sql}");
        let result = MutableExecutablePlan::Command(result)
            .finish(&control)
            .unwrap();
        assert_eq!(
            normalize(&result),
            normalize(&UnifiedPlan::Command(expected)),
            "{sql}"
        );
        drop(result);
        assert_eq!(budget.used(), 0, "{sql}");
    }
}

#[test]
fn native_values_actions_move_large_literals_without_copying_them() {
    let catalog = Catalog(vec![ColumnDef::nullable("a", ColumnType::Text)]);
    let original = command(&format!(
        "INSERT INTO log(a) VALUES ('{}'), (DEFAULT)",
        "x".repeat(4 << 20)
    ));
    let budget = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &token);
    let original = original.copy_for_rewrite(&control).unwrap();
    let CommandPlan::Insert(insert) = &*original else {
        panic!("INSERT")
    };
    let ScalarExpr::Literal(Value::Str(value)) = &insert.rows[0][0] else {
        panic!("payload")
    };
    let pointer = value.as_ptr();
    let before = budget.used();
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        result = Some(
            RuleValuesInput::prepare_action_with_control(catalog.analysis(), original, &control)
                .unwrap(),
        );
    });
    let result = result.unwrap();
    assert!(measured.bytes_total < 128 * 1024, "{measured:?}");
    assert_eq!(before as i64 + measured.bytes_current, budget.used() as i64);
    let CommandPlan::Insert(insert) = &*result else {
        panic!("INSERT")
    };
    let mut count = 0;
    insert
        .source
        .as_ref()
        .unwrap()
        .visit_scalar_expressions(&mut |expr| {
            expr.visit(&mut |node| {
                if let ScalarExpr::Literal(Value::Str(value)) = node {
                    if value.len() == 4 << 20 {
                        assert_eq!(value.as_ptr(), pointer);
                        count += 1;
                    }
                }
            });
        });
    assert_eq!(count, 1);
    drop(result);
    assert_eq!(budget.used(), 0);
}

#[test]
fn native_values_admission_failure_and_both_cancellations_release_consumed_inputs() {
    let mut column = ColumnDef::nullable("a", ColumnType::Text);
    column.default = Some(Expr::Literal(Value::Str("x".repeat(4 << 20))));
    let catalog = Catalog(vec![column]);
    let original = command("INSERT INTO log(a) VALUES (DEFAULT)");
    let budget = MemoryBudget::new(32 << 10);
    let sibling = budget.reserve(37).unwrap();
    let first = CancellationToken::new();
    let second = CancellationToken::new();
    let control = ProductionControl::new(&budget, &first, &second);
    let input = original.copy_for_rewrite(&control).unwrap();
    let measured = allocation_counter::measure(|| {
        assert_eq!(
            RuleValuesInput::prepare_action_with_control(catalog.analysis(), input, &control)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(measured.bytes_total < 16 << 10, "{measured:?}");
    assert_eq!(budget.used(), sibling.bytes());
    for token in [&first, &second] {
        let input = original.copy_for_rewrite(&control).unwrap();
        token.cancel();
        let selected = command("INSERT INTO log(a) SELECT 1");
        let CommandPlan::Insert(selected) = &*selected else {
            panic!("INSERT")
        };
        assert_eq!(
            RuleValuesInput::describe_with_control(selected, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(
            RuleValuesInput::prepare_action_with_control(catalog.analysis(), input, &control)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(budget.used(), sibling.bytes());
        token.reset();
    }
    let small = catalog_fixture();
    let input = original.copy_for_rewrite(&control).unwrap();
    drop(RuleValuesInput::prepare_action_with_control(small.analysis(), input, &control).unwrap());
    assert_eq!(budget.used(), sibling.bytes());
}

#[test]
fn native_rule_input_attachment_preserves_every_relational_action_shape() {
    let catalog = catalog_fixture();
    for sql in [
        "SELECT 1 WHERE 1 = (SELECT 1)",
        "VALUES ((SELECT 1)), ((SELECT 2))",
        "VALUES ((SELECT 1))",
        "INSERT INTO log (a) VALUES (DEFAULT), ((SELECT 2))",
        "INSERT INTO log (a) VALUES (DEFAULT)",
        "INSERT INTO log (a) SELECT 1 WHERE 1 = (SELECT 1)",
        "UPDATE log SET a = 1 WHERE a = (SELECT 1)",
        "UPDATE log SET a = 1 FROM other WHERE a = (SELECT 1)",
        "DELETE FROM log WHERE a = (SELECT 1)",
        "DELETE FROM log USING other WHERE a = (SELECT 1)",
    ] {
        for inline in [false, true] {
            let action = crate::compile(sql).unwrap().remove(0);
            let UnifiedPlan::Query(query) = UnifiedPlan::lower(
                crate::compile("SELECT 7 AS n WHERE 1 = (SELECT 1)")
                    .unwrap()
                    .remove(0),
            ) else {
                panic!("input")
            };
            let relation = InternalRelationId::allocate();
            let condition = ExpressionPlan::lower(expression("true = (SELECT true)"));
            let expected = RuleAction::bind(
                catalog.analysis(),
                &|_: &str| false,
                action.clone(),
                Some(RuleQueryInput {
                    query: (*query).clone(),
                    inline,
                    relation,
                    condition: Some(condition.clone()),
                    expressions: BTreeMap::new(),
                }),
            )
            .unwrap()
            .lower(&|_: &str| false)
            .unwrap();
            let budget = MemoryBudget::new(4 << 20);
            let token = CancellationToken::new();
            let control = ProductionControl::new(&budget, &token, &token);
            let plan = UnifiedPlan::lower_borrowed_for_rewrite(&action, &|_: &str| false, &control)
                .unwrap()
                .mark_catalog_relations_bound(&control)
                .unwrap();
            let input = OwnedRuleQueryInput {
                query: query.copy_for_rewrite(&control).unwrap(),
                inline,
                relation,
                condition: Some(condition.copy_for_rewrite(&control).unwrap()),
                expressions: control
                    .finish(
                        uqa_core::memory::OwnedMap::new(),
                        control.empty_reservation(),
                    )
                    .unwrap(),
            };
            let before = budget.used();
            let mut actual = None;
            let measured = allocation_counter::measure(|| {
                actual = Some(input.attach(plan, catalog.analysis(), &control).unwrap());
            });
            let actual = actual.unwrap();
            assert_eq!(
                before as i64 + measured.bytes_current,
                budget.used() as i64,
                "{sql} inline={inline}"
            );
            assert_eq!(actual.reserved_bytes(), budget.used());
            let actual = actual.finish(&control).unwrap();
            assert_eq!(
                normalize(&actual),
                normalize(&expected),
                "{sql} inline={inline}"
            );
            drop(actual);
            assert_eq!(budget.used(), 0);
        }
    }
}
