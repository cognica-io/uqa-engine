//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish changed composite datums through the existing owners of stored schema expressions and catalog registries.

use crate::schema::{
    events::EventCatalogContext,
    indexes::{registry::IndexRegistryPublication, routines::IndexRoutineContext},
    publication::dependencies::SchemaDependencyPublicationContext,
};
use std::collections::BTreeSet;
use uqa_sql::{expr::composites::constants::CompositeConstantChange, SQLError};

pub struct CompositeCatalogValueContext<'a> {
    pub schema: SchemaDependencyPublicationContext<'a>,
    pub domains: &'a dyn crate::catalog::domain::DomainRegistryPublication,
    pub events: EventCatalogContext<'a>,
    pub routines: crate::routines::catalog::RoutineMutationContext<'a>,
    pub indexes: IndexRoutineContext<'a>,
    pub index_publication: &'a dyn IndexRegistryPublication,
    pub types: &'a dyn uqa_sql::type_resolution::FunctionTypeResolver,
}

fn storage(error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("rewrite stored composite constants: {error}"))
}

pub(super) fn rewrite(
    context: &CompositeCatalogValueContext<'_>,
    change: &CompositeConstantChange<'_>,
) -> Result<BTreeSet<String>, SQLError> {
    tables(&context.schema, change)?;
    foreign(&context.schema, change)?;
    domains(context.domains, change)?;
    events(&context.events, change)?;
    crate::routines::catalog::rewrite_composite_constants(&context.routines, change)?;
    indexes(context, change)
}

fn tables(
    context: &SchemaDependencyPublicationContext<'_>,
    change: &CompositeConstantChange<'_>,
) -> Result<(), SQLError> {
    let tables = context.tables.table_schemas();
    let mut updates = Vec::new();
    for (position, (name, table)) in tables.iter().enumerate() {
        let mut columns = table.columns();
        let mut constraints = table.constraints();
        let mut changed = change.columns(&mut columns)?;
        changed |= change.checks(&mut constraints.checks, &columns)?;
        if let Some(bound) = &mut constraints.hierarchy.partition_bound {
            let parent_name = constraints
                .hierarchy
                .parents
                .first()
                .ok_or_else(|| storage("partition has no parent"))?;
            let parent = tables
                .iter()
                .find(|(name, _)| name == parent_name)
                .ok_or_else(|| storage("partition parent disappeared"))?;
            let hierarchy = parent.1.hierarchy();
            let spec = hierarchy
                .partition_spec
                .as_ref()
                .ok_or_else(|| storage("partition parent has no key"))?;
            changed |= change.bound(bound, spec, &parent.1.columns())?;
        }
        if let Some(spec) = &mut constraints.hierarchy.partition_spec {
            changed |= change.spec(spec)?;
        }
        if changed {
            table
                .persist_candidate(&columns, &constraints)
                .map_err(storage)?;
            updates.push((position, columns, constraints));
            let relation = uqa_core::RelationIdentity::from_legacy_name(name).map_err(storage)?;
            context.changes.prepared_relation_changed(&relation);
        }
    }
    if !updates.is_empty() {
        for (position, columns, constraints) in updates {
            tables[position].1.publish_constraints(columns, constraints);
        }
        context.changes.table_catalog_changed();
    }
    Ok(())
}

fn foreign(
    context: &SchemaDependencyPublicationContext<'_>,
    change: &CompositeConstantChange<'_>,
) -> Result<(), SQLError> {
    let mut updates = Vec::new();
    for (name, mut table) in context.foreign.foreign_tables() {
        let mut changed = change.columns(&mut table.columns)?;
        changed |= change.checks(&mut table.checks, &table.columns)?;
        if changed {
            context
                .foreign
                .persist_foreign_table(&name, &table)
                .map_err(storage)?;
            context.changes.prepared_relation_changed(&name);
            updates.push((name, table));
        }
    }
    if !updates.is_empty() {
        context.foreign.publish_foreign_tables(updates);
        context.changes.catalog_registry_changed();
    }
    Ok(())
}

fn domains(
    context: &dyn crate::catalog::domain::DomainRegistryPublication,
    change: &CompositeConstantChange<'_>,
) -> Result<(), SQLError> {
    let before = context.domain_registry().clone();
    let mut next = before.clone();
    let mut changed = false;
    for domain in next.values_mut() {
        for expression in domain.definition.default.iter_mut().chain(
            domain
                .definition
                .checks
                .iter_mut()
                .map(|check| &mut check.expression),
        ) {
            changed |= change.expression(expression)?;
        }
    }
    if changed {
        crate::catalog::domain::publish(context, &before, next)?;
    }
    Ok(())
}

fn events(
    context: &EventCatalogContext<'_>,
    change: &CompositeConstantChange<'_>,
) -> Result<(), SQLError> {
    let mut triggers = context.registry.triggers().clone();
    let mut rules = context.registry.rules().clone();
    let mut triggers_changed = false;
    let mut rules_changed = false;
    for trigger in triggers
        .values_mut()
        .flat_map(|entries| entries.values_mut())
    {
        if let Some(expression) = &mut trigger.definition.when {
            triggers_changed |= change.expression(expression)?;
        }
    }
    for rule in rules.values_mut().flat_map(|entries| entries.values_mut()) {
        if let Some(expression) = &mut rule.definition.condition {
            rules_changed |= change.expression(expression)?;
        }
        if let Some(plan) = &mut rule.condition_plan {
            rules_changed |= change.expression_plan(plan)?;
        }
        for statement in &mut rule.definition.actions {
            rules_changed |= change.statement(statement)?;
        }
    }
    if triggers_changed {
        context.publication.persist_triggers(&triggers)?;
        **context.registry.triggers() = triggers;
    }
    if rules_changed {
        context.publication.persist_rules(&rules)?;
        **context.registry.rules() = rules;
    }
    if triggers_changed || rules_changed {
        context.changes.catalog_registry_changed();
    }
    Ok(())
}

fn indexes(
    context: &CompositeCatalogValueContext<'_>,
    change: &CompositeConstantChange<'_>,
) -> Result<BTreeSet<String>, SQLError> {
    let mut tables = BTreeSet::new();
    let mut updates = Vec::new();
    let rows = context.indexes.registry.routine_index_rows();
    for row in rows.values() {
        let mut keys: Vec<uqa_sql::ast::IndexKey> =
            serde_json::from_str(&row.columns_json).map_err(storage)?;
        let mut definition = crate::catalog::index::index_definition(row).map_err(storage)?;
        let mut changed = false;
        for key in &mut keys {
            if let uqa_sql::ast::IndexKey::Expression(expression) = key {
                changed |= change.expression(expression)?;
            }
        }
        if let Some(predicate) = &mut definition.predicate {
            changed |= change.expression(predicate)?;
        }
        if changed {
            let mut row = row.clone();
            row.columns_json = serde_json::to_string(&keys).map_err(storage)?;
            row.definition_json = Some(serde_json::to_string(&definition).map_err(storage)?);
            updates.push(row);
        }
    }
    drop(rows);
    for row in updates {
        context
            .index_publication
            .persist_index(&row)
            .map_err(storage)?;
        tables.insert(row.table_name.clone());
        context.index_publication.publish_index(row);
    }
    Ok(tables)
}
