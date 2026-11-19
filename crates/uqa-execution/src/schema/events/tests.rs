//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::{Cell, RefCell};
use uqa_core::memory::MemoryBudget;
use uqa_sql::{
    ast::{EventEnableMode, Statement},
    catalog::events::{RuleDependencies, StoredRule, StoredTrigger},
};

struct Catalogs {
    triggers: RefCell<TriggerCatalog>,
    rules: RefCell<RuleCatalog>,
    writes: Cell<usize>,
    changes: Cell<usize>,
}
impl EventCatalogGuards for Catalogs {
    fn triggers(&self) -> TriggerCatalogWrite<'_> {
        Box::new(self.triggers.borrow_mut())
    }
    fn rules(&self) -> RuleCatalogWrite<'_> {
        Box::new(self.rules.borrow_mut())
    }
}
impl EventCatalogPublication for Catalogs {
    fn persist_triggers(&self, _: &TriggerCatalog) -> Result<(), SQLError> {
        self.writes.set(self.writes.get() + 1);
        Ok(())
    }
    fn persist_rules(&self, _: &RuleCatalog) -> Result<(), SQLError> {
        self.writes.set(self.writes.get() + 1);
        Ok(())
    }
    fn finish_publication(&self) -> Result<(), SQLError> {
        assert!(self.triggers.try_borrow_mut().is_ok());
        assert!(self.rules.try_borrow_mut().is_ok());
        Ok(())
    }
}
impl CatalogPublicationChanges for Catalogs {
    fn table_catalog_changed(&self) {
        panic!("event registry publication")
    }
    fn catalog_registry_changed(&self) {
        self.changes.set(self.changes.get() + 1);
    }
}

fn definitions(
    trigger_memory: &MemoryBudget,
    rule_memory: &MemoryBudget,
    from: &RelationIdentity,
) -> (TriggerCatalog, RuleCatalog) {
    let mut triggers = TriggerCatalog::with_budget(trigger_memory);
    let Statement::CreateTrigger(definition) = uqa_sql::compile(
        "CREATE TRIGGER watch BEFORE INSERT ON public.items FOR EACH ROW EXECUTE FUNCTION fire()",
    )
    .unwrap()
    .remove(0) else {
        panic!("trigger")
    };
    triggers
        .insert_event(
            from.clone(),
            "watch".into(),
            StoredTrigger {
                definition,
                function_object_id: Some([1; 16]),
                enabled: EventEnableMode::Always,
                object_id: Some([2; 16]),
                constraint_name: None,
                catalog_oid: Some(30_001),
                constraint_catalog_oid: None,
            },
        )
        .unwrap();
    let mut rules = RuleCatalog::with_budget(rule_memory);
    let Statement::CreateRule(definition) =
        uqa_sql::compile("CREATE RULE watch AS ON DELETE TO public.items DO NOTHING")
            .unwrap()
            .remove(0)
    else {
        panic!("rule")
    };
    rules
        .insert_event(
            from.clone(),
            "watch".into(),
            StoredRule {
                definition,
                enabled: EventEnableMode::Replica,
                condition_plan: None,
                condition_binding: None,
                dependencies: Some(RuleDependencies::default()),
                catalog_oid: Some(40_001),
            },
        )
        .unwrap();
    (triggers, rules)
}

#[test]
fn event_rename_admits_both_catalogs_before_persistence_and_keeps_typed_failure() {
    let trigger_memory = MemoryBudget::new(1 << 20);
    let rule_memory = MemoryBudget::new(1 << 20);
    let from = RelationIdentity::new("public", "items");
    let to = RelationIdentity::new("public", "renamed");
    let (triggers, rules) = definitions(&trigger_memory, &rule_memory, &from);
    let original_triggers = triggers.clone();
    let original_rules = rules.clone();
    let baseline = (trigger_memory.used(), rule_memory.used());
    let catalogs = Catalogs {
        triggers: RefCell::new(triggers),
        rules: RefCell::new(rules),
        writes: Cell::new(0),
        changes: Cell::new(0),
    };
    let context = EventCatalogContext {
        registry: &catalogs,
        publication: &catalogs,
        changes: &catalogs,
    };
    let exhausted = rule_memory.reserve(rule_memory.available()).unwrap();
    let error = rename_relation_events(&context, &from, &to).unwrap_err();
    assert_eq!(
        uqa_sql::catalog::errors::storage_error("event rename", &error).sqlstate(),
        Some("53200")
    );
    assert_eq!(catalogs.writes.get(), 0);
    assert_eq!(catalogs.changes.get(), 0);
    assert!(std::ptr::eq(
        &raw const catalogs.triggers.borrow()[&from]["watch"],
        &raw const original_triggers[&from]["watch"]
    ));
    assert!(std::ptr::eq(
        &raw const catalogs.rules.borrow()[&from]["watch"],
        &raw const original_rules[&from]["watch"]
    ));
    drop(exhausted);
    assert_eq!((trigger_memory.used(), rule_memory.used()), baseline);
    rename_relation_events(&context, &from, &to).unwrap();
    assert_eq!(catalogs.writes.get(), 2);
    assert_eq!(catalogs.changes.get(), 1);
    assert!(!catalogs.triggers.borrow().contains_key(&from));
    assert!(!catalogs.rules.borrow().contains_key(&from));
    assert_eq!(
        catalogs.triggers.borrow()[&to]["watch"].definition.table,
        "public.renamed"
    );
    assert_eq!(
        catalogs.rules.borrow()[&to]["watch"].definition.table,
        "public.renamed"
    );
    assert_eq!(
        original_triggers[&from]["watch"].definition.table,
        "public.items"
    );
    assert_eq!(
        original_rules[&from]["watch"].definition.table,
        "public.items"
    );
    drop((catalogs, original_triggers, original_rules));
    assert_eq!((trigger_memory.used(), rule_memory.used()), (0, 0));
}
