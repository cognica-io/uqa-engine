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
    pub bindings: &'a dyn uqa_sql::routines::compilation::RoutineCompilationCatalog,
    pub resolution: &'a dyn uqa_sql::routines::RoutineResolution,
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
            if change.rename.is_some() {
                table.persist_expression_names(&columns, &constraints)
            } else {
                table.persist_candidate(&columns, &constraints)
            }
            .map_err(storage)?;
            updates.push((position, columns, constraints));
            if change.rename.is_none() {
                let relation =
                    uqa_core::RelationIdentity::from_legacy_name(name).map_err(storage)?;
                context.changes.prepared_relation_changed(&relation);
            }
        }
    }
    if !updates.is_empty() {
        for (position, columns, constraints) in updates {
            tables[position]
                .1
                .publish_constraints(columns, constraints)
                .map_err(|error| {
                    crate::storage_errors::storage_error(
                        "rewrite stored composite definitions",
                        &error,
                    )
                })?;
        }
        context.changes.table_catalog_changed();
    }
    Ok(())
}

fn foreign(
    context: &SchemaDependencyPublicationContext<'_>,
    change: &CompositeConstantChange<'_>,
) -> Result<(), SQLError> {
    let original = context.foreign.foreign_catalog();
    let mut updates = Vec::new();
    for (name, table) in original.tables() {
        let mut table = table.clone();
        let mut changed = change.columns(&mut table.columns)?;
        changed |= change.checks(&mut table.checks, &table.columns)?;
        if changed {
            updates.push((name.clone(), table));
        }
    }
    let next = original.with_table_updates(&updates)?;
    for (name, table) in &updates {
        context
            .foreign
            .persist_foreign_table(name, table)
            .map_err(storage)?;
        context.changes.prepared_relation_changed(name);
    }
    if !updates.is_empty() {
        context.foreign.publish_foreign_tables(next, updates);
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
    for (name, original) in before.iter() {
        let mut domain = original.clone();
        let mut rewritten = false;
        let schema = uqa_sql::RowSchema::with_types(
            vec!["value".into()],
            vec![Some(domain.definition.base.clone())],
        );
        for expression in domain.definition.default.iter_mut().chain(
            domain
                .definition
                .checks
                .iter_mut()
                .map(|check| &mut check.expression),
        ) {
            rewritten |= change.expression_in_schema(expression, &schema)?;
        }
        if rewritten {
            next.insert(name.clone(), domain)?;
            changed = true;
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
    let triggers_changed = triggers.rewrite_composite_constants(change)?;
    let rules_changed = rules.rewrite_composite_constants(change)?;
    if triggers_changed {
        context.publication.persist_triggers(&triggers)?;
    }
    if rules_changed {
        context.publication.persist_rules(&rules)?;
    }
    if triggers_changed {
        **context.registry.triggers() = triggers;
    }
    if rules_changed {
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
        let schema = change
            .rename
            .map_or(Ok(uqa_sql::RowSchema::default()), |rename| {
                rename.relation_schema(&row.table_name)
            })?;
        let mut keys: Vec<uqa_sql::ast::IndexKey> =
            serde_json::from_str(&row.columns_json).map_err(storage)?;
        let mut definition = crate::catalog::index::index_definition(row).map_err(storage)?;
        let mut changed = false;
        for key in &mut keys {
            if let uqa_sql::ast::IndexKey::Expression(expression) = key {
                changed |= change.expression_in_schema(expression, &schema)?;
            }
        }
        if let Some(predicate) = &mut definition.predicate {
            changed |= change.expression_in_schema(predicate, &schema)?;
        }
        if changed {
            let mut row = row.clone();
            row.columns_json = serde_json::to_string(&keys).map_err(storage)?;
            row.definition_json = Some(serde_json::to_string(&definition).map_err(storage)?);
            updates.push(row);
        }
    }
    drop(rows);
    if updates.is_empty() {
        return Ok(tables);
    }
    let mut candidate = context.index_publication.index_catalog().as_ref().clone();
    for row in &updates {
        candidate.replace(row.clone()).map_err(|error| {
            crate::storage_errors::storage_error("rewrite stored composite constants", &error)
        })?;
    }
    for row in updates {
        context
            .index_publication
            .persist_index(
                &row,
                if change.rename.is_some() {
                    crate::schema::indexes::registry::IndexPublicationKind::Names
                } else {
                    crate::schema::indexes::registry::IndexPublicationKind::Definition
                },
            )
            .map_err(|error| {
                crate::storage_errors::storage_error("rewrite stored composite constants", &error)
            })?;
        tables.insert(row.table_name.clone());
    }
    context.index_publication.publish_indexes(candidate);
    Ok(tables)
}
