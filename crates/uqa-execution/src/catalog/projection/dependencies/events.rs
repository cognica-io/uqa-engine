//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of triggers and rules as `CreateTriggerFiringOn` and `InsertRule` record them.

use super::{ColumnScope, DependencyBuilder, MemberObject, References};
use crate::catalog::projection::events::{
    catalog_triggers, rule_catalog_oid, trigger_catalog_oid, trigger_constraint_catalog_oid,
};
use uqa_sql::ast::{RuleEvent, Statement};
use uqa_sql::catalog::dependencies::{
    DependencyKind, ObjectAddress, CONSTRAINT_CLASS, PROCEDURE_CLASS, RELATION_CLASS,
    REWRITE_CLASS, TRIGGER_CLASS,
};
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    /// A trigger depends normally on its function; an ordinary trigger goes with its table and, for a constraint trigger, the referenced table, and its constraint is part of it; a partition's clone belongs to its parent's trigger and to the partition; `UPDATE OF` columns and what `WHEN` uses are normal dependencies.
    pub(super) fn record_triggers(&mut self) -> Result<(), SQLError> {
        for (trigger, parent) in catalog_triggers(self.catalog, self.resolution)? {
            let definition = &trigger.definition;
            let Some(relation) = self.objects.relation_oid_by_name(&definition.table) else {
                continue;
            };
            let oid = super::catalog_oid(trigger_catalog_oid(
                self.catalog,
                self.resolution,
                &trigger,
            )?)?;
            self.objects.add_member(
                TRIGGER_CLASS,
                oid,
                MemberObject::Trigger {
                    name: definition.name.clone(),
                    relation,
                },
            );
            let address = ObjectAddress::whole(TRIGGER_CLASS, oid);
            if let Some(function) = self.trigger_function_oid(&trigger) {
                self.recorder.record(
                    address,
                    ObjectAddress::whole(PROCEDURE_CLASS, function),
                    DependencyKind::Normal,
                );
            }
            self.recorder.record(
                address,
                ObjectAddress::whole(RELATION_CLASS, relation),
                DependencyKind::Auto,
            );
            if let Some(referenced) = definition
                .referenced_table
                .as_deref()
                .and_then(|name| self.objects.relation_oid_by_name(name))
            {
                self.recorder.record(
                    address,
                    ObjectAddress::whole(RELATION_CLASS, referenced),
                    DependencyKind::Auto,
                );
            }
            if definition.constraint {
                self.record_trigger_constraint(&trigger, address, relation)?;
            }
            if let Ok(parent @ 1..) = u32::try_from(parent) {
                self.recorder.record(
                    address,
                    ObjectAddress::whole(TRIGGER_CLASS, parent),
                    DependencyKind::PartitionPrimary,
                );
                self.recorder.record(
                    address,
                    ObjectAddress::whole(RELATION_CLASS, relation),
                    DependencyKind::PartitionSecondary,
                );
            }
            let table = self.relation_object(relation)?.clone();
            for column in &definition.update_columns {
                if let Some(number) = table.column_number(column) {
                    self.recorder.record(
                        address,
                        ObjectAddress::column(relation, number),
                        DependencyKind::Normal,
                    );
                }
            }
            if let Some(condition) = &definition.when {
                let mut references = References::default();
                self.expressions().collect(
                    condition,
                    ColumnScope::Trigger(relation, &table),
                    &mut references,
                )?;
                self.recorder
                    .record_references(address, references, DependencyKind::Normal);
            }
        }
        Ok(())
    }

    /// A user constraint trigger's constraint names no columns, so it goes with its table, and it is part of the trigger.
    fn record_trigger_constraint(
        &mut self,
        trigger: &uqa_sql::catalog::events::StoredTrigger,
        address: ObjectAddress,
        relation: u32,
    ) -> Result<(), SQLError> {
        let oid = super::catalog_oid(trigger_constraint_catalog_oid(
            self.catalog,
            self.resolution,
            trigger,
        )?)?;
        self.objects.add_member(
            CONSTRAINT_CLASS,
            oid,
            MemberObject::Constraint {
                name: trigger
                    .constraint_name
                    .clone()
                    .unwrap_or_else(|| trigger.definition.name.clone()),
                owner: super::ConstraintOwner::Relation(relation),
                not_null: false,
            },
        );
        let constraint = ObjectAddress::whole(CONSTRAINT_CLASS, oid);
        self.recorder.record(
            constraint,
            ObjectAddress::whole(RELATION_CLASS, relation),
            DependencyKind::Auto,
        );
        self.recorder
            .record(constraint, address, DependencyKind::Internal);
        Ok(())
    }

    fn trigger_function_oid(
        &self,
        trigger: &uqa_sql::catalog::events::StoredTrigger,
    ) -> Option<u32> {
        if let Some(object_id) = &trigger.function_object_id {
            return self.objects.routine_oid(object_id);
        }
        // A trigger stored before routine identities were recorded names its function, which takes no arguments.
        let name = &trigger.definition.function;
        self.catalog
            .all_sql_functions()
            .into_iter()
            .find(|function| {
                function.def.name == *name && function.def.identity_params().is_empty()
            })
            .and_then(|function| self.objects.routine_oid(function.def.object_id.as_ref()?))
    }

    /// A rule goes with its relation, and depends normally on what its actions and qualification use. The `OLD` and `NEW` entries of a data-changing action's range table reference the relation too.
    pub(super) fn record_rules(&mut self) -> Result<(), SQLError> {
        for rule in self.catalog.rules() {
            let definition = &rule.definition;
            let Some(relation) = self.objects.relation_oid_by_name(&definition.table) else {
                continue;
            };
            let oid = super::catalog_oid(rule_catalog_oid(&rule))?;
            self.objects.add_member(
                REWRITE_CLASS,
                oid,
                MemberObject::Rule {
                    name: definition.name.clone(),
                    relation,
                },
            );
            let address = ObjectAddress::whole(REWRITE_CLASS, oid);
            let kind = if definition.event == RuleEvent::Select {
                DependencyKind::Internal
            } else {
                DependencyKind::Auto
            };
            self.recorder.record(
                address,
                ObjectAddress::whole(RELATION_CLASS, relation),
                kind,
            );
            let mut references = References::default();
            if definition
                .actions
                .iter()
                .any(|action| !matches!(action, Statement::Notify { .. }))
            {
                references.add_relation(relation);
            }
            if let Some(dependencies) = &rule.dependencies {
                for referenced in &dependencies.relations {
                    if let Some(oid) = self.objects.relation_oid(referenced) {
                        references.add_relation(oid);
                    }
                }
                for column in &dependencies.columns {
                    let Some(oid) = self.objects.relation_oid(&column.relation) else {
                        continue;
                    };
                    if let Some(number) = self
                        .objects
                        .relation(oid)
                        .and_then(|relation| relation.column_number(&column.column))
                    {
                        references.add_column(oid, number);
                    }
                }
                for routine in &dependencies.routines {
                    if let Some(oid) = routine
                        .object_id
                        .as_ref()
                        .and_then(|object_id| self.objects.routine_oid(object_id))
                    {
                        references.add_routine(oid);
                    }
                }
            }
            let expressions = self.expressions();
            let mut types = Vec::new();
            let table = self.relation_object(relation)?;
            let transitions = super::composite_fields::transition_schema(&table.columns);
            for action in &definition.actions {
                for address in
                    uqa_sql::binding::composite_dependencies::statement_composite_dependencies(
                        self.context.routines,
                        action,
                        &self.field_binding_context(),
                        &transitions,
                    )?
                {
                    references.add(address);
                }
                types.extend(uqa_sql::catalog::stored_ast::stored_statement_type_names(
                    action,
                )?);
            }
            if let Some(condition) = &definition.condition {
                expressions.collect_composite_fields(
                    condition,
                    ColumnScope::Trigger(relation, table),
                    &mut references,
                )?;
                types.extend(uqa_sql::catalog::stored_ast::stored_expression_type_names(
                    condition,
                )?);
            }
            for name in &types {
                if let Some(oid) = expressions.type_oid(name) {
                    references.add_type(oid);
                }
            }
            self.recorder
                .record_references(address, references, DependencyKind::Normal);
        }
        Ok(())
    }
}
