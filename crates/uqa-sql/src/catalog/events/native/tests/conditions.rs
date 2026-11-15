//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::InternalRelationId, catalog::events::RuleConditionBinding, ScalarExpr};

fn captured() -> StoredRule {
    let mut value =
        rule("CREATE RULE saved AS ON UPDATE TO public.items WHERE old.id IS NOT NULL DO NOTHING");
    let old = InternalRelationId::from_raw(101);
    value.condition_binding = Some(RuleConditionBinding::from_captured_parts(
        Some(old),
        Some(InternalRelationId::from_raw(202)),
        vec!["id".into(), "unused".into()],
    ));
    value.condition_plan = Some(ExpressionPlan {
        scalar: ScalarExpr::IsNull {
            expr: Box::new(ScalarExpr::InternalColumn(old.column(0))),
            negated: true,
        },
        subqueries: Vec::new(),
    });
    value
}

#[test]
fn native_rule_conditions_reject_unowned_captured_row_references() {
    for (relation, attribute, valid) in [
        (101, 0, true),
        (999, 0, false),
        (101, 2, false),
        (101, 1, false),
    ] {
        let mut value = captured();
        let ScalarExpr::IsNull { expr, .. } = &mut value.condition_plan.as_mut().unwrap().scalar
        else {
            panic!("captured condition");
        };
        **expr =
            ScalarExpr::InternalColumn(InternalRelationId::from_raw(relation).column(attribute));
        let before = serde_json::to_vec(&value).unwrap();
        let definitions = rules(value);
        assert_eq!(
            check(&TriggerCatalog::new(), &definitions).is_ok(),
            valid,
            "relation {relation}, attribute {attribute}"
        );
        assert_eq!(
            serde_json::to_vec(&definitions.values().next().unwrap()["saved"]).unwrap(),
            before
        );
    }
}

#[test]
fn native_rule_conditions_require_complete_distinct_event_row_bindings() {
    for broken in [
        "plan",
        "binding",
        "definition",
        "same",
        "zero",
        "side",
        "columns",
    ] {
        let mut value = captured();
        match broken {
            "plan" => value.condition_plan = None,
            "binding" => value.condition_binding = None,
            "definition" => value.definition.condition = None,
            "side" => value.definition.event = crate::ast::RuleEvent::Insert,
            _ => {
                let (old, new, columns) = match broken {
                    "same" => (101, 101, vec!["id".into()]),
                    "zero" => (0, 202, vec!["id".into()]),
                    "columns" => (101, 202, vec!["id".into(), "id".into()]),
                    _ => unreachable!(),
                };
                value.condition_binding = Some(RuleConditionBinding::from_captured_parts(
                    Some(InternalRelationId::from_raw(old)),
                    Some(InternalRelationId::from_raw(new)),
                    columns,
                ));
            }
        }
        assert!(
            check(&TriggerCatalog::new(), &rules(value)).is_err(),
            "{broken}"
        );
    }
}

fn replace_column(value: &mut serde_json::Value, replacement: &serde_json::Value) -> usize {
    if value == &serde_json::json!({"Column":"old.id"})
        || value == &serde_json::json!({"QualifiedColumn":{"qualifier":"old","column":"id"}})
    {
        *value = replacement.clone();
        return 1;
    }
    match value {
        serde_json::Value::Array(values) => values
            .iter_mut()
            .map(|v| replace_column(v, replacement))
            .sum(),
        serde_json::Value::Object(values) => values
            .values_mut()
            .map(|v| replace_column(v, replacement))
            .sum(),
        _ => 0,
    }
}

#[test]
fn native_rule_conditions_visit_correlated_and_cte_query_row_references() {
    for condition in [
        "EXISTS(SELECT 1 FROM public.target WHERE id=old.id)",
        "EXISTS(WITH x AS (SELECT old.id AS id) SELECT 1 FROM x WHERE id>0)",
        "EXISTS(SELECT 1 WHERE EXISTS(SELECT 1 FROM public.target WHERE id=old.id))",
    ] {
        for relation in [101, 999] {
            let mut value = captured();
            let source = rule(&format!(
                "CREATE RULE saved AS ON UPDATE TO public.items WHERE {condition} DO NOTHING"
            ));
            let plan = ExpressionPlan::lower(source.definition.condition.unwrap());
            let mut encoded = serde_json::to_value(plan).unwrap();
            let reference = serde_json::to_value(ScalarExpr::InternalColumn(
                InternalRelationId::from_raw(relation).column(0),
            ))
            .unwrap();
            assert_eq!(replace_column(&mut encoded, &reference), 1, "{encoded}");
            value.condition_plan = Some(serde_json::from_value(encoded).unwrap());
            assert_eq!(
                check(&TriggerCatalog::new(), &rules(value)).is_ok(),
                relation == 101,
                "{condition}"
            );
        }
    }
}

#[test]
fn native_rule_conditions_reject_execution_only_implicit_row_inputs() {
    let mut value = captured();
    let plan = value.condition_plan.as_mut().unwrap();
    let mut binding = crate::ast::FunctionBinding::builtin_by_name("merge_action", None);
    binding.dispatch = Some(crate::ast::FunctionDispatch::MergeAction);
    plan.scalar = ScalarExpr::Func {
        name: "merge_action".into(),
        binding: Some(binding),
        order_syntax: crate::ast::FunctionOrderSyntax::Ordinary,
        args: Vec::new(),
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    };
    assert!(check(&TriggerCatalog::new(), &rules(value)).is_err());
}

#[test]
fn native_rule_condition_work_retires_memory_on_denial_and_both_cancellations() {
    let value = captured();
    let binding = value.condition_binding.as_ref().unwrap();
    let plan = value.condition_plan.as_ref().unwrap();
    let before = serde_json::to_vec(&value).unwrap();
    for (limit, cancel) in [
        (0, "none"),
        (1 << 20, "none"),
        (1 << 20, "original"),
        (1 << 20, "invoking"),
        (1 << 20, "active"),
    ] {
        let memory = MemoryBudget::new(limit);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        if cancel == "original" {
            original.cancel();
        }
        if cancel == "invoking" {
            invoking.cancel();
        }
        let control = ProductionControl::new(&memory, &original, &invoking);
        let mut observed = 0;
        let result =
            binding.validate_captured(value.definition.event, plan, &control, &mut |name| {
                assert_eq!(name, "id");
                observed += 1;
                if cancel == "active" {
                    invoking.cancel();
                }
                Ok(())
            });
        assert_eq!(
            result.is_ok(),
            limit != 0 && cancel == "none",
            "{limit}: {cancel}"
        );
        if cancel == "active" || result.is_ok() {
            assert_eq!(observed, 1);
        }
        assert_eq!(memory.used(), 0);
        assert_eq!(serde_json::to_vec(&value).unwrap(), before);
    }
}
