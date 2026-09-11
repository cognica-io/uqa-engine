//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{fixtures::Catalog, EventAnalysisContext, RelationIdentity};
use crate::{
    ast::{Statement, TableHierarchy},
    catalog::events::{
        definition::lookup::{EventLookupContext, EventPartitionCatalog},
        reads::{EventCatalogReads, RuleCatalogRead, TriggerCatalogRead},
        RuleCatalog, StoredTrigger, TriggerCatalog,
    },
};
use std::{cell::RefCell, collections::BTreeMap};
struct Partitions {
    hierarchy: BTreeMap<RelationIdentity, TableHierarchy>,
    triggers: TriggerCatalog,
    rules: RuleCatalog,
    reads: RefCell<Vec<String>>,
}
impl EventPartitionCatalog for Partitions {
    fn contains_loaded_table(&self, relation: &RelationIdentity) -> bool {
        self.reads
            .borrow_mut()
            .push(format!("loaded:{}", relation.qualified_name()));
        self.hierarchy.contains_key(relation)
    }
    fn table_names(&self) -> Result<Vec<String>, String> {
        self.reads.borrow_mut().push("names".into());
        Ok(self
            .hierarchy
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect())
    }
    fn try_table_hierarchy(&self, name: &str) -> Result<TableHierarchy, String> {
        self.reads.borrow_mut().push(format!("hierarchy:{name}"));
        self.hierarchy
            .get(&RelationIdentity::from_legacy_name(name)?)
            .cloned()
            .ok_or_else(|| "missing hierarchy".into())
    }
}
impl EventCatalogReads for Partitions {
    fn read_rules(&self) -> RuleCatalogRead<'_> {
        Box::new(&self.rules)
    }
    fn read_triggers(&self) -> TriggerCatalogRead<'_> {
        self.reads.borrow_mut().push("triggers".into());
        Box::new(&self.triggers)
    }
}
impl Partitions {
    fn context<'a>(&'a self, analysis: &EventAnalysisContext<'a>) -> EventLookupContext<'a> {
        EventLookupContext {
            analysis: *analysis,
            partitions: self,
            registry: self,
        }
    }
}
fn hierarchy(sql: &str) -> TableHierarchy {
    let Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected table")
    };
    table.hierarchy
}
fn fixture() -> Partitions {
    Partitions {
        hierarchy: BTreeMap::from([
            (
                RelationIdentity::new("public", "parent"),
                hierarchy("CREATE TABLE parent (id integer) PARTITION BY RANGE (id)"),
            ),
            (
                RelationIdentity::new("public", "child"),
                hierarchy("CREATE TABLE child PARTITION OF parent FOR VALUES FROM (0) TO (10)"),
            ),
        ]),
        triggers: TriggerCatalog::new(),
        rules: RuleCatalog::new(),
        reads: RefCell::new(Vec::new()),
    }
}
fn trigger(table: &str, name: &str) -> StoredTrigger {
    let Statement::CreateTrigger(definition) = crate::compile(&format!(
        "CREATE TRIGGER {name} BEFORE INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION handler()"
    ))
    .unwrap()
    .remove(0) else {
        panic!("expected trigger")
    };
    StoredTrigger {
        definition,
        function_object_id: Some([4; 16]),
        enabled: crate::ast::EventEnableMode::Origin,
        object_id: Some([5; 16]),
        constraint_name: None,
    }
}
#[test]
fn invalid_partition_parent_fails_before_name_enumeration_or_trigger_reads() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    partitions
        .hierarchy
        .get_mut(&RelationIdentity::new("public", "child"))
        .unwrap()
        .parents
        .clear();
    let error = partitions
        .context(&catalog.context())
        .ensure_partition_trigger_name_available(
            &RelationIdentity::new("public", "child"),
            "same",
            false,
        )
        .unwrap_err();
    assert!(
        matches!(error,crate::SQLError::Internal(message) if message=="partition `public.child` has no parent")
    );
    assert_eq!(
        *partitions.reads.borrow(),
        ["loaded:public.child", "hierarchy:public.child"]
    );
}
#[test]
fn ancestor_names_are_checked_even_when_replacing_a_local_trigger() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    partitions.triggers.insert(
        RelationIdentity::new("public", "parent"),
        BTreeMap::from([("same".into(), trigger("public.parent", "same"))]),
    );
    let error = partitions
        .context(&catalog.context())
        .ensure_partition_trigger_name_available(
            &RelationIdentity::new("public", "child"),
            "same",
            true,
        )
        .unwrap_err();
    assert!(
        matches!(error,crate::SQLError::Routine {sqlstate,message} if sqlstate=="42710" && message=="trigger \"same\" for relation \"public.child\" already exists")
    );
    let reads = partitions.reads.borrow();
    assert_eq!(reads.last().unwrap(), "triggers");
    assert!(
        reads.iter().position(|read| read == "names").unwrap()
            < reads.iter().position(|read| read == "triggers").unwrap()
    );
}
#[test]
fn descendant_conflicts_report_the_descendant_relation() {
    let catalog = Catalog::default();
    let mut partitions = fixture();
    partitions.triggers.insert(
        RelationIdentity::new("public", "child"),
        BTreeMap::from([("same".into(), trigger("public.child", "same"))]),
    );
    let error = partitions
        .context(&catalog.context())
        .ensure_partition_trigger_name_available(
            &RelationIdentity::new("public", "parent"),
            "same",
            false,
        )
        .unwrap_err();
    assert!(
        matches!(error,crate::SQLError::Routine {sqlstate,message} if sqlstate=="42710" && message=="trigger \"same\" for relation \"public.child\" already exists")
    );
    assert_eq!(partitions.reads.borrow().last().unwrap(), "triggers");
}
