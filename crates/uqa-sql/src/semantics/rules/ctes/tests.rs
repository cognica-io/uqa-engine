//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::EventEnableMode, catalog::events::StoredRule};
use std::collections::BTreeMap;
use uqa_core::RelationIdentity;

#[derive(Default)]
struct Catalog(BTreeMap<String, Vec<StoredRule>>);

impl Catalog {
    fn add(&mut self, sql: &str) {
        let Statement::CreateRule(definition) = crate::compile(sql).unwrap().remove(0) else {
            panic!("rule definition");
        };
        self.0
            .entry(definition.table.clone())
            .or_default()
            .push(StoredRule {
                definition,
                enabled: EventEnableMode::Origin,
                condition_plan: None,
                condition_binding: None,
                dependencies: None,
                catalog_oid: None,
            });
    }
}

impl RuleCatalog for Catalog {
    fn relation_has_rules(&self, table: &str) -> Result<bool, SQLError> {
        Ok(self.0.contains_key(table))
    }
    fn resolve_rule_relation(&self, table: &str) -> Result<RelationIdentity, SQLError> {
        RelationIdentity::from_legacy_name(table).map_err(SQLError::Internal)
    }
    fn rules_for(&self, table: &str, event: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        Ok(self
            .0
            .get(table)
            .into_iter()
            .flatten()
            .filter(|rule| rule.definition.event == event)
            .cloned()
            .collect())
    }
    fn resolve_mutation_target(&self, name: &str, _bound: bool) -> Result<String, SQLError> {
        Ok(name.into())
    }
}

#[test]
fn cte_rewrite_counts_surviving_queries_and_nested_actions() {
    for (rules, expected) in [
        (Vec::new(), None),
        (vec!["CREATE RULE r AS ON INSERT TO public.input DO INSTEAD NOTHING"], None),
        (vec!["CREATE RULE r AS ON INSERT TO public.input DO INSTEAD INSERT INTO public.output VALUES(NEW.n)"], None),
        (vec!["CREATE RULE r AS ON INSERT TO public.input DO ALSO INSERT INTO public.output VALUES(NEW.n)"], Some("0A000")),
        (vec!["CREATE RULE r AS ON INSERT TO public.input WHERE NEW.n > 0 DO INSTEAD INSERT INTO public.output VALUES(NEW.n)"], Some("0A000")),
        (vec!["CREATE RULE r AS ON INSERT TO public.input DO INSTEAD INSERT INTO public.hop VALUES(NEW.n)", "CREATE RULE r AS ON INSERT TO public.hop DO INSTEAD (INSERT INTO public.output VALUES(NEW.n); INSERT INTO public.output VALUES(NEW.n))"], Some("0A000")),
        (vec!["CREATE RULE r AS ON INSERT TO public.input DO INSTEAD INSERT INTO public.hop VALUES(NEW.n)", "CREATE RULE r AS ON INSERT TO public.hop DO INSTEAD INSERT INTO public.input VALUES(NEW.n)"], Some("42P17")),
    ] {
        let mut catalog = Catalog::default();
        for sql in rules { catalog.add(sql); }
        let result = validate_rule_cte_contract(&catalog, "public.input", RuleEvent::Insert, |_, _| Ok(None));
        assert_eq!(result.as_ref().err().and_then(SQLError::sqlstate), expected, "{result:?}");
    }
}

#[test]
fn cte_rewrite_traverses_deep_chains_without_call_stack_growth_and_obeys_cancellation() {
    let mut catalog = Catalog::default();
    for index in 0..1024 {
        catalog.add(&format!("CREATE RULE r AS ON INSERT TO public.r{index} DO INSTEAD INSERT INTO public.r{} VALUES(NEW.n)", index + 1));
    }
    validate_rule_cte_contract(&catalog, "public.r0", RuleEvent::Insert, |_, _| Ok(None)).unwrap();
    let token = uqa_core::CancellationToken::new();
    token.cancel();
    let _scope = crate::expr::StatementCancellationScope::enter(token);
    let error =
        validate_rule_cte_contract(&catalog, "public.r0", RuleEvent::Insert, |_, _| Ok(None))
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
}

#[test]
fn cte_rewrite_counts_automatic_views_only_after_base_rules() {
    for (rule, expected) in [
        ("CREATE RULE r AS ON INSERT TO public.base DO INSTEAD NOTHING", None),
        ("CREATE RULE r AS ON INSERT TO public.base DO INSTEAD INSERT INTO public.output VALUES(NEW.n)", Some("0A000")),
    ] {
        let mut catalog = Catalog::default();
        catalog.add("CREATE RULE view_action AS ON INSERT TO public.view DO ALSO INSERT INTO public.output VALUES(NEW.n)");
        catalog.add(rule);
        let result = validate_rule_cte_contract(&catalog, "public.view", RuleEvent::Insert, |table, _| {
            Ok((table == "public.view").then(|| "public.base".into()))
        });
        assert_eq!(result.as_ref().err().and_then(SQLError::sqlstate), expected);
    }
}
