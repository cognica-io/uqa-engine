//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collect and remove domain dependents using live table guards and ordered catalog publication.

use crate::catalog::foreign::StoredForeignTable;
use crate::schema::{
    namespaces::NamespaceCatalogChanges,
    removal::{RelationRemovalEvents, RelationRemovalLocks},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Deref,
    sync::Arc,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnDef, FunctionBinding, TableCheck},
    catalog::{domain::StoredDomain, stored_view::StoredView},
    schema::domains::dependencies::{self as analysis, DomainDependents, DomainTypeCatalog},
    SQLError,
};
use uqa_storage::{CatalogIndexRow, StorageBackendError, StorageBackendResult};

pub type DomainColumnRead<'a> = Box<dyn Deref<Target = Vec<ColumnDef>> + 'a>;
pub type DomainCheckRead<'a> = Box<dyn Deref<Target = Vec<TableCheck>> + 'a>;
pub trait DomainTableMetadata {
    fn domain_columns(&self) -> DomainColumnRead<'_>;
    fn domain_table_checks(&self) -> DomainCheckRead<'_>;
}
pub trait DomainDependencyCatalog {
    fn domain_definitions(&self) -> BTreeMap<String, StoredDomain>;
    fn domain_index_rows(&self) -> BTreeMap<RelationIdentity, CatalogIndexRow>;
    fn domain_table_schemas(&self) -> Vec<(String, Arc<dyn DomainTableMetadata>)>;
    fn domain_foreign_tables(&self) -> BTreeMap<RelationIdentity, StoredForeignTable>;
    fn domain_view_definitions(&self) -> BTreeMap<RelationIdentity, StoredView>;
}
pub trait DomainViewDependencies {
    fn views_depending_on_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Vec<String>>;
    fn cascade_view_closure(&self, names: Vec<String>) -> Result<Vec<String>, SQLError>;
    fn drop_views_inner(&self, names: &[String], cascade: bool) -> Result<(), SQLError>;
}
pub trait DomainRegistryPublication {
    fn persist_domain_definitions(
        &self,
        registry: &BTreeMap<String, StoredDomain>,
    ) -> Result<(), SQLError>;
    fn publish_domain_definitions(&self, registry: BTreeMap<String, StoredDomain>);
}
pub trait DomainTableRemoval {
    fn drop_constraint_dependency(&self, table: &str, name: &str) -> Result<(), SQLError>;
    fn clear_column_default(&self, table: &str, column: &str) -> StorageBackendResult<()>;
    fn drop_column_cascade(
        &self,
        table: &str,
        column: &str,
        if_exists: bool,
    ) -> Result<(), SQLError>;
}
pub trait DomainForeignRemoval {
    fn drop_foreign_table_check_dependency(
        &self,
        table: &str,
        name: &str,
    ) -> StorageBackendResult<()>;
    fn clear_foreign_table_default_dependency(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<()>;
    fn drop_foreign_table_column_dependency(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<()>;
}
pub trait DomainIndexRemoval {
    fn drop_index_dependency(&self, relation: &RelationIdentity) -> Result<(), SQLError>;
}
pub struct DomainDependencyContext<'a> {
    pub types: &'a dyn DomainTypeCatalog,
    pub catalog: &'a dyn DomainDependencyCatalog,
    pub views: &'a dyn DomainViewDependencies,
    pub publication: &'a dyn DomainRegistryPublication,
    pub tables: &'a dyn DomainTableRemoval,
    pub foreign: &'a dyn DomainForeignRemoval,
    pub indexes: &'a dyn DomainIndexRemoval,
    pub events: &'a dyn RelationRemovalEvents,
    pub locks: &'a dyn RelationRemovalLocks,
    pub changes: &'a dyn NamespaceCatalogChanges,
}

fn storage_error(error: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("drop domain dependency: {error}"))
}

pub fn domain_drop_column_names(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
) -> Result<BTreeSet<(String, String)>, SQLError> {
    Ok(domain_drop_dependents(context, targets)?
        .columns
        .into_iter()
        .map(|(table, column, _)| (table, column))
        .collect())
}

pub fn domain_drop_has_dependents(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
) -> Result<bool, SQLError> {
    let dependents = domain_drop_dependents(context, targets)?;
    if !dependents.indexes.is_empty()
        || !dependents.columns.is_empty()
        || !dependents.defaults.is_empty()
        || !dependents.checks.is_empty()
        || !domain_dependent_view_names(context, targets, &dependents)?.is_empty()
    {
        return Ok(true);
    }
    for domain in context
        .catalog
        .domain_definitions()
        .values()
        .filter(|domain| !targets.contains(&domain.oid))
    {
        for check in &domain.definition.checks {
            if analysis::expression_references_domain(context.types, &check.expression, targets)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn domain_drop_dependents(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
) -> Result<DomainDependents, SQLError> {
    let mut dependents = DomainDependents {
        indexes: domain_dependent_indexes(context, targets)?,
        ..DomainDependents::default()
    };
    for (table, state) in context.catalog.domain_table_schemas() {
        analysis::domain_schema_dependents(
            context.types,
            &table,
            &state.domain_columns(),
            &state.domain_table_checks(),
            false,
            targets,
            &mut dependents,
        )?;
    }
    for (identity, table) in context.catalog.domain_foreign_tables() {
        analysis::domain_schema_dependents(
            context.types,
            &identity.qualified_name(),
            &table.columns,
            &table.checks,
            true,
            targets,
            &mut dependents,
        )?;
    }
    Ok(dependents)
}

pub fn domain_drop_view_names(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
) -> Result<Vec<String>, SQLError> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    domain_dependent_view_names(context, targets, &domain_drop_dependents(context, targets)?)
}

fn drop_domain_view_dependents(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
    dependents: &DomainDependents,
) -> Result<(), SQLError> {
    let closure = domain_dependent_view_names(context, targets, dependents)?;
    context
        .events
        .drop_rules_depending_on_relations_inner(&closure)
        .map_err(|error| storage_error(&error))?;
    context.views.drop_views_inner(&closure, false)
}

fn drop_domain_schema_dependents(
    context: &DomainDependencyContext<'_>,
    dependents: &DomainDependents,
) -> Result<(), SQLError> {
    for index in &dependents.indexes {
        context.indexes.drop_index_dependency(index)?;
    }
    let mut tables = BTreeSet::new();
    tables.extend(dependents.columns.iter().map(|(table, _, _)| table));
    tables.extend(dependents.defaults.iter().map(|(table, _, _)| table));
    tables.extend(dependents.checks.iter().map(|(table, _, _)| table));
    for table in tables {
        context.locks.lock_exclusive(table)?;
        context
            .events
            .ensure_no_pending_trigger_events(table, "ALTER TABLE")?;
    }
    for (table, constraint, foreign) in &dependents.checks {
        if *foreign {
            context
                .foreign
                .drop_foreign_table_check_dependency(table, constraint)
                .map_err(|error| storage_error(&error))?;
        } else {
            context
                .tables
                .drop_constraint_dependency(table, constraint)?;
        }
    }
    for (table, column, foreign) in &dependents.defaults {
        if *foreign {
            context
                .foreign
                .clear_foreign_table_default_dependency(table, column)
                .map_err(|error| storage_error(&error))?;
        } else {
            context
                .tables
                .clear_column_default(table, column)
                .map_err(|error| storage_error(&error))?;
        }
    }
    for (table, column, foreign) in &dependents.columns {
        if *foreign {
            context
                .foreign
                .drop_foreign_table_column_dependency(table, column)
                .map_err(|error| storage_error(&error))?;
        } else {
            context.tables.drop_column_cascade(table, column, true)?;
        }
    }
    Ok(())
}

pub fn expand_domain_drop_targets(
    context: &DomainDependencyContext<'_>,
    targets: &mut BTreeSet<u32>,
    routines: &[FunctionBinding],
) -> Result<(), SQLError> {
    let registry = context.catalog.domain_definitions();
    analysis::expand_domain_drop_targets(context.types, &registry, targets, routines)
}

pub fn domain_checks_depending_on_routines(
    context: &DomainDependencyContext<'_>,
    routines: &[FunctionBinding],
) -> Result<Vec<(String, String)>, SQLError> {
    analysis::domain_checks_depending_on_routines(context.catalog.domain_definitions(), routines)
}

pub fn drop_domain_routine_checks(
    context: &DomainDependencyContext<'_>,
    routines: &[FunctionBinding],
) -> Result<(), SQLError> {
    let checks = domain_checks_depending_on_routines(context, routines)?;
    if checks.is_empty() {
        return Ok(());
    }
    let mut registry = context.catalog.domain_definitions();
    analysis::remove_domain_routine_checks(&mut registry, checks)?;
    context.publication.persist_domain_definitions(&registry)?;
    context.publication.publish_domain_definitions(registry);
    context.changes.catalog_registry_changed();
    Ok(())
}

pub fn commit_domain_drop(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
) -> Result<(), SQLError> {
    if targets.is_empty() {
        return Ok(());
    }
    let mut registry = context.catalog.domain_definitions();
    let dependents = domain_drop_dependents(context, targets)?;
    drop_domain_view_dependents(context, targets, &dependents)?;
    drop_domain_schema_dependents(context, &dependents)?;
    analysis::remove_domain_references(context.types, &mut registry, targets)?;
    context.publication.persist_domain_definitions(&registry)?;
    context.publication.publish_domain_definitions(registry);
    context.changes.catalog_registry_changed();
    Ok(())
}

fn domain_dependent_view_names(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
    dependents: &DomainDependents,
) -> Result<Vec<String>, SQLError> {
    let mut views = analysis::views_referencing_domains(
        context.types,
        context.catalog.domain_view_definitions(),
        targets,
    );
    for (table, column, _) in &dependents.columns {
        views.extend(
            context
                .views
                .views_depending_on_column(table, column)
                .map_err(|error| storage_error(&error))?,
        );
    }
    context
        .views
        .cascade_view_closure(views.into_iter().collect())
}

fn domain_dependent_indexes(
    context: &DomainDependencyContext<'_>,
    targets: &BTreeSet<u32>,
) -> Result<BTreeSet<RelationIdentity>, SQLError> {
    let mut indexes = BTreeSet::new();
    for row in context.catalog.domain_index_rows().values() {
        let definition =
            crate::catalog::index::index_definition(row).map_err(|error| storage_error(&error))?;
        let keys = analysis::parse_domain_index_keys(&row.columns_json)?;
        if analysis::index_references_domain(context.types, &definition, &keys, targets)? {
            indexes.insert(row.relation.clone());
        }
    }
    Ok(indexes)
}
