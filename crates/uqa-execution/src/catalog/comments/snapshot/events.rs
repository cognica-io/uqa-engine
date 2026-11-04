//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    BTreeSet, RelationPersistence, SQLError, Scope, Selection, CONSTRAINT_CLASS, TRIGGER_CLASS,
};
use uqa_sql::catalog::events::StoredTrigger;

fn exclude(
    source: &mut Selection,
    trigger: &StoredTrigger,
    table: &str,
    relation_oid: u32,
    recorded: bool,
    classes: [bool; 2],
) -> Result<(), SQLError> {
    let relation_oid = || Ok(i64::from(relation_oid));
    if classes[0] {
        let oid = crate::catalog::projection::trigger_catalog_oid_for_relation(
            trigger,
            table,
            recorded.then_some(trigger.catalog_oid).flatten(),
            relation_oid,
        )?;
        source.exclude(TRIGGER_CLASS, oid as u32);
    }
    if classes[1] && trigger.definition.constraint {
        let oid = crate::catalog::projection::trigger_constraint_catalog_oid_for_relation(
            trigger,
            table,
            recorded.then_some(trigger.constraint_catalog_oid).flatten(),
            relation_oid,
        )?;
        source.exclude(CONSTRAINT_CLASS, oid as u32);
    }
    Ok(())
}

impl Scope<'_, '_> {
    pub(super) fn exclude_events(&self, source: &mut Selection) -> Result<(), SQLError> {
        let classes = [
            source.has_class(TRIGGER_CLASS),
            source.has_class(CONSTRAINT_CLASS),
        ];
        if !classes.into_iter().any(|present| present) {
            return Ok(());
        }
        for (relation, triggers) in self.definitions.triggers.iter() {
            let Some(relation_oid) = self.temporary_relation_oid(relation) else {
                continue;
            };
            for trigger in triggers.values() {
                exclude(
                    source,
                    trigger,
                    &trigger.definition.table,
                    relation_oid,
                    true,
                    classes,
                )?;
            }
        }
        for table in self.tables.values().filter(|table| {
            table.persistence == RelationPersistence::Temporary && table.hierarchy.is_partition()
        }) {
            let ancestors = crate::catalog::hierarchy::partition_trigger_sources(
                table.relation.clone(),
                |relation| self.table(relation).map(|table| table.hierarchy),
            )?;
            let mut names = BTreeSet::new();
            if let Some(own) = self.definitions.triggers.get(table.relation) {
                names.extend(own.values().map(|trigger| trigger.definition.name.as_str()));
            }
            let table_name = table.relation.qualified_name();
            for ancestor in ancestors.iter().skip(1) {
                let Some(triggers) = self.definitions.triggers.get(ancestor) else {
                    continue;
                };
                for trigger in triggers.values().filter(|trigger| trigger.definition.row) {
                    if names.insert(trigger.definition.name.as_str()) {
                        exclude(
                            source,
                            trigger,
                            &table_name,
                            table.oids().relation,
                            false,
                            classes,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }
}
