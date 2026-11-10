//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::Statement,
    catalog::events::{RuleColumnDependency, RuleDependencies, StoredTrigger},
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

struct Catalog;

impl NativeEventCatalog for Catalog {
    fn kind(&self, relation: &RelationIdentity) -> Result<Option<&'static str>, SQLError> {
        Ok(match (relation.schema.as_str(), relation.name.as_str()) {
            ("public", "items" | "target" | "new") => Some("table"),
            ("public", "visible") => Some("view"),
            ("public", "remote") => Some("foreign table"),
            _ => None,
        })
    }

    fn contains_column(
        &self,
        _: &RelationIdentity,
        column: &str,
        control: &ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        Ok(column == "id")
    }
}

fn rule(sql: &str) -> StoredRule {
    let Statement::CreateRule(definition) = crate::compile(sql).unwrap().remove(0) else {
        panic!("rule");
    };
    StoredRule {
        definition,
        enabled: crate::ast::EventEnableMode::default(),
        condition_plan: None,
        condition_binding: None,
        dependencies: None,
        catalog_oid: Some(31_000),
    }
}

fn rules(rule: StoredRule) -> RuleCatalog {
    let mut rules = RuleCatalog::new();
    rules
        .insert_event(
            RelationIdentity::new("public", "items"),
            "saved".into(),
            rule,
        )
        .unwrap();
    rules
}

fn trigger() -> StoredTrigger {
    let Statement::CreateTrigger(definition) = crate::compile(
        "CREATE TRIGGER saved BEFORE UPDATE OF id ON public.items FOR EACH ROW EXECUTE FUNCTION public.fire()",
    ).unwrap().remove(0) else { panic!("trigger"); };
    StoredTrigger {
        definition,
        function_object_id: None,
        enabled: crate::ast::EventEnableMode::default(),
        object_id: None,
        constraint_name: None,
        catalog_oid: Some(32_000),
        constraint_catalog_oid: None,
    }
}

fn check(triggers: &TriggerCatalog, rules: &RuleCatalog) -> Result<(), SQLError> {
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let result = validate(
        triggers,
        rules,
        &Catalog,
        &ProductionControl::new(&memory, &token, &token),
    );
    assert_eq!(memory.used(), 0);
    result
}

#[test]
fn native_events_validate_original_registry_owners_and_column_references() {
    for broken in [
        "none",
        "name",
        "owner",
        "missing",
        "column",
        "reference",
        "kind",
    ] {
        let mut trigger = trigger();
        let mut owner = RelationIdentity::new("public", "items");
        match broken {
            "name" => trigger.definition.name = "different".into(),
            "owner" => trigger.definition.table = "public.target".into(),
            "missing" => {
                owner.name = "absent".into();
                trigger.definition.table = "public.absent".into();
            }
            "column" => trigger.definition.update_columns[0] = "absent".into(),
            "reference" => trigger.definition.referenced_table = Some("public.absent".into()),
            "kind" => {
                owner.name = "visible".into();
                trigger.definition.table = "public.visible".into();
            }
            _ => {}
        }
        let mut triggers = TriggerCatalog::new();
        triggers
            .insert_event(owner, "saved".into(), trigger)
            .unwrap();
        assert_eq!(
            check(&triggers, &RuleCatalog::new()).is_ok(),
            broken == "none",
            "{broken}"
        );
    }
    for owner in ["public.visible", "public.remote"] {
        let mut trigger = trigger();
        trigger.definition.table = owner.into();
        if owner.ends_with("visible") {
            trigger.definition.row = false;
        }
        let mut triggers = TriggerCatalog::new();
        triggers
            .insert_event(
                RelationIdentity::from_legacy_name(owner).unwrap(),
                "saved".into(),
                trigger,
            )
            .unwrap();
        check(&triggers, &RuleCatalog::new()).unwrap();
    }
}

#[test]
fn native_rules_validate_recorded_relation_and_column_dependencies() {
    for broken in [
        "none",
        "name",
        "owner",
        "relation",
        "column_relation",
        "column",
    ] {
        let mut rule = rule("CREATE RULE saved AS ON INSERT TO public.items DO NOTHING");
        let mut dependencies = RuleDependencies::default();
        dependencies.relations.insert(RelationIdentity::new(
            "public",
            if broken == "relation" {
                "missing"
            } else {
                "target"
            },
        ));
        dependencies.columns.insert(RuleColumnDependency {
            relation: RelationIdentity::new(
                "public",
                if broken == "column_relation" {
                    "missing"
                } else {
                    "items"
                },
            ),
            column: if broken == "column" { "missing" } else { "id" }.into(),
        });
        rule.dependencies = Some(dependencies);
        if broken == "name" {
            rule.definition.name = "different".into();
        }
        if broken == "owner" {
            rule.definition.table = "public.target".into();
        }
        assert_eq!(
            check(&TriggerCatalog::new(), &rules(rule)).is_ok(),
            broken == "none",
            "{broken}"
        );
    }
}

#[test]
fn native_rule_actions_keep_pseudo_rows_ctes_and_concrete_targets_distinct() {
    for (action, valid) in [
        ("INSERT INTO public.target VALUES (new.id)", true),
        ("INSERT INTO public.missing VALUES (new.id)", false),
        ("SELECT id FROM new", false),
        ("SELECT id FROM old", false),
        ("WITH x AS (SELECT id FROM new) SELECT * FROM x", false),
        (
            "WITH x AS (SELECT id FROM public.missing) SELECT * FROM x",
            false,
        ),
        (
            "WITH RECURSIVE x AS (SELECT 1 UNION ALL SELECT id FROM x) SELECT * FROM x",
            true,
        ),
        ("INSERT INTO new VALUES (1)", false),
        (
            "WITH new AS (SELECT id FROM public.target) SELECT * FROM new",
            true,
        ),
        ("INSERT INTO public.new VALUES (1)", true),
        ("SELECT id FROM public.new", true),
    ] {
        let rule = rule(&format!(
            "CREATE RULE saved AS ON INSERT TO public.items DO ALSO {action}"
        ));
        let before = serde_json::to_vec(&rule).unwrap();
        let rules = rules(rule);
        assert_eq!(
            check(&TriggerCatalog::new(), &rules).is_ok(),
            valid,
            "{action}"
        );
        assert_eq!(
            serde_json::to_vec(&rules.values().next().unwrap()["saved"]).unwrap(),
            before
        );
    }
}

#[test]
fn native_rule_conditions_preserve_legacy_and_bound_reference_validation() {
    for bound in [false, true] {
        for target in ["public.target", "public.missing"] {
            let mut rule = rule(&format!("CREATE RULE saved AS ON UPDATE TO public.items WHERE new.id IN (SELECT id FROM {target}) DO NOTHING"));
            if bound {
                rule.condition_plan = Some(ExpressionPlan::lower(
                    rule.definition.condition.as_ref().unwrap().clone(),
                ));
                rule.definition.condition = None;
            }
            assert_eq!(
                check(&TriggerCatalog::new(), &rules(rule)).is_ok(),
                target == "public.target"
            );
        }
    }
}

#[test]
fn native_event_reference_work_retires_original_admission_on_failure_and_cancellation() {
    let rules = rules(rule("CREATE RULE saved AS ON INSERT TO public.items DO ALSO INSERT INTO public.target VALUES (new.id)"));
    let triggers = TriggerCatalog::new();
    let mut passed = false;
    for limit in [0, 128, 1024, 4096, 1 << 20] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let result = validate(
            &triggers,
            &rules,
            &Catalog,
            &ProductionControl::new(&memory, &token, &token),
        );
        passed |= result.is_ok();
        assert_eq!(memory.used(), 0);
        token.cancel();
        assert!(validate(
            &triggers,
            &rules,
            &Catalog,
            &ProductionControl::new(&memory, &token, &token)
        )
        .is_err());
        assert_eq!(memory.used(), 0);
    }
    assert!(passed);
}
