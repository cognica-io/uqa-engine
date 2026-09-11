//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Event dependency publication through retained registry guards and pending transaction state.
use super::context::EventLifecycleContext;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{DropRule, DropTrigger},
    catalog::events::PreparedRuleColumnDrop,
    SQLError,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};
impl EventLifecycleContext<'_> {
    pub fn drop_rules_depending_on_relations_inner(
        &self,
        relations: &[String],
    ) -> StorageBackendResult<()> {
        let dependents = self
            .lookup
            .rules_depending_on_relations(relations)
            .map_err(StorageBackendError::Other)?;
        if dependents.is_empty() {
            return Ok(());
        }
        let mut rules = self.catalog.registry.rules();
        let next = uqa_sql::catalog::events::removal::removed_dependent_rules(&rules, &dependents)
            .map_err(StorageBackendError::Other)?;
        self.catalog
            .publication
            .persist_rules(&next)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        **rules = next;
        drop(rules);
        for (event_relation, name) in dependents {
            self.notice(
                "NOTICE",
                &format!(
                    "drop cascades to rule {name} on table {}",
                    event_relation.qualified_name()
                ),
            );
        }
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn rewrite_event_routine_identity(
        &self,
        target: &uqa_sql::ast::FunctionBinding,
        new_name: &str,
    ) -> Result<(), SQLError> {
        let mut next_triggers = self.lookup.registry.read_triggers().clone();
        let triggers_changed =
            uqa_sql::catalog::events::definition::rewrites::rewrite_trigger_routine_references(
                &mut next_triggers,
                target,
                new_name,
            )?;
        if triggers_changed {
            self.catalog.publication.persist_triggers(&next_triggers)?;
            **self.catalog.registry.triggers() = next_triggers;
        }

        let mut next_rules = self.lookup.registry.read_rules().clone();
        let rules_changed = self.lookup.analysis.rewrite_rule_routine_references(
            &mut next_rules,
            target,
            new_name,
        )?;
        if rules_changed {
            self.catalog.publication.persist_rules(&next_rules)?;
            **self.catalog.registry.rules() = next_rules;
        }
        if triggers_changed || rules_changed {
            self.catalog.changes.catalog_registry_changed();
        }
        Ok(())
    }

    pub fn drop_relation_events_inner(
        &self,
        relation: &RelationIdentity,
    ) -> StorageBackendResult<()> {
        let mut triggers = self.catalog.registry.triggers();
        let mut rules = self.catalog.registry.rules();
        let Some(uqa_sql::catalog::events::removal::RemovedRelationEvents {
            triggers: next_triggers,
            rules: next_rules,
            constraints: removed_constraint_identities,
        }) =
            uqa_sql::catalog::events::removal::removed_relation_events(&triggers, &rules, relation)
                .map_err(StorageBackendError::Other)?
        else {
            return Ok(());
        };
        self.catalog
            .publication
            .persist_triggers(&next_triggers)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.catalog
            .publication
            .persist_rules(&next_rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        **triggers = next_triggers;
        **rules = next_rules;
        drop(rules);
        drop(triggers);
        for identity in &removed_constraint_identities {
            self.pending.forget(identity);
        }
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn rename_event_column_inner(
        &self,
        table: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(table).map_err(StorageBackendError::Other)?;
        let triggers = self.lookup.registry.read_triggers().clone();
        let rules = self.lookup.registry.read_rules().clone();
        let Some((next_triggers, next_rules)) = self
            .lookup
            .analysis
            .renamed_event_column(&triggers, &rules, &relation, from, to)
            .map_err(StorageBackendError::Other)?
        else {
            return Ok(());
        };
        self.catalog
            .publication
            .persist_triggers(&next_triggers)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        self.catalog
            .publication
            .persist_rules(&next_rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        **self.catalog.registry.triggers() = next_triggers;
        **self.catalog.registry.rules() = next_rules;
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn finish_rule_column_drop(
        &self,
        mut prepared: PreparedRuleColumnDrop,
    ) -> StorageBackendResult<()> {
        if prepared.rebind.is_empty() {
            return Ok(());
        }
        self.lookup
            .analysis
            .rebind_rule_column_drop(&mut prepared)
            .map_err(StorageBackendError::Other)?;
        self.catalog
            .publication
            .persist_rules(&prepared.rules)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        **self.catalog.registry.rules() = prepared.rules;
        self.catalog.changes.catalog_registry_changed();
        Ok(())
    }

    pub fn handle_drop_column_event_dependencies(
        &self,
        table: &str,
        column: &str,
        cascade: bool,
    ) -> Result<(), SQLError> {
        let uqa_sql::catalog::events::definition::dependents::EventColumnDependencies {
            triggers: dependent_triggers,
            rules: dependent_rules,
        } = self.lookup.column_event_dependencies(table, column)?;
        if dependent_triggers.is_empty() && dependent_rules.is_empty() {
            return Ok(());
        }
        if !cascade {
            let mut objects = dependent_triggers
                .iter()
                .map(|name| format!("trigger {name}"))
                .collect::<Vec<_>>();
            objects.extend(
                dependent_rules
                    .iter()
                    .map(|(_, name)| format!("rule {name}")),
            );
            return Err(SQLError::Routine {
                sqlstate: "2BP01".into(),
                message: format!(
                    "cannot drop column {column} of table {table} because {} depends on it",
                    objects.join(", ")
                ),
            });
        }
        for name in dependent_triggers {
            self.drop_trigger(&DropTrigger {
                name: name.clone(),
                table: table.to_string(),
                if_exists: false,
                cascade: true,
            })?;
            self.notice(
                "NOTICE",
                &format!("drop cascades to trigger {name} on table {table}"),
            );
        }
        for (event_relation, name) in dependent_rules {
            let event_table = event_relation.qualified_name();
            self.drop_rule(&DropRule {
                name: name.clone(),
                table: event_table.clone(),
                if_exists: false,
                cascade: true,
            })?;
            self.notice(
                "NOTICE",
                &format!("drop cascades to rule {name} on table {event_table}"),
            );
        }
        Ok(())
    }
}
