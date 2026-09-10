//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-table ALTER execution and publication through retained catalog write guards.
use super::{
    publication::dependencies::CatalogPublicationChanges,
    relation_alteration::{
        rewrite_relation_rename_dependents, role_transfer_target, RelationAlterLocks,
        RelationRenameDependencies, RoleTransferContext,
    },
    sequences::role_ownership::{
        table_owned_sequence_owner_updates, OwnedSequenceSecurityCatalog,
        OwnedSequenceSecurityWrite, SequenceSecurityPublication,
    },
};
use crate::catalog::{foreign::StoredForeignTable, security::TableSecurity};
use std::{collections::BTreeMap, ops::DerefMut};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterForeignTableAction, AlterForeignTableStmt},
    catalog::security::table::{rewrite_acl_owner, validate_table_security_invariants},
    schema::relation_alteration::{self, RelationAlterNames},
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub type ForeignTableRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, StoredForeignTable>> + 'a>;
pub type ForeignSecurityRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, TableSecurity>> + 'a>;
pub type ForeignMemoryRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, Vec<uqa_fdw::Row>>> + 'a>;

pub trait ForeignTableAlterCatalog {
    fn contains_table(&self, relation: &RelationIdentity) -> bool;
    fn security(&self, relation: &RelationIdentity) -> Option<TableSecurity>;
    fn table(&self, relation: &RelationIdentity) -> Option<StoredForeignTable>;
}
pub trait ForeignTableAlterAccess {
    fn ensure_owner(&self, name: &str) -> Result<String, SQLError>;
}
pub trait ForeignTableOwnerWriter {
    fn prepare_writer(&self) -> Result<(), SQLError>;
}
pub trait ForeignTableAlterPublication {
    fn persist_rename(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<Option<bool>>;
    fn tables_write(&self) -> ForeignTableRegistryWrite<'_>;
    fn security_write(&self) -> ForeignSecurityRegistryWrite<'_>;
    fn memory_tables_write(&self) -> ForeignMemoryRegistryWrite<'_>;
    fn sequence_security_write(&self) -> OwnedSequenceSecurityWrite<'_>;
    fn persist_security(
        &self,
        relation: &RelationIdentity,
        security: &TableSecurity,
    ) -> Result<(), SQLError>;
}
pub struct ForeignTableAlterContext<'a> {
    pub names: &'a dyn RelationAlterNames,
    pub catalog: &'a dyn ForeignTableAlterCatalog,
    pub access: &'a dyn ForeignTableAlterAccess,
    pub locks: &'a dyn RelationAlterLocks,
    pub writer: &'a dyn ForeignTableOwnerWriter,
    pub roles: RoleTransferContext<'a>,
    pub dependencies: &'a dyn RelationRenameDependencies,
    pub owned_sequences: &'a dyn OwnedSequenceSecurityCatalog,
    pub sequence_publication: &'a dyn SequenceSecurityPublication,
    pub publication: &'a dyn ForeignTableAlterPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}
pub type ForeignTableAlterWrite<'a> =
    Box<dyn FnOnce(&ForeignTableAlterContext<'_>) -> Result<(), SQLError> + 'a>;
pub trait ForeignTableAlterTransactions {
    fn with_foreign_table_write(&self, write: ForeignTableAlterWrite<'_>) -> Result<(), SQLError>;
}

pub fn bound_foreign_table_security(
    catalog: &dyn ForeignTableAlterCatalog,
    name: &str,
) -> Result<(RelationIdentity, TableSecurity), SQLError> {
    let relation = RelationIdentity::from_legacy_name(name)
        .map_err(|error| SQLError::Internal(format!("resolve foreign table `{name}`: {error}")))?;
    if !catalog.contains_table(&relation) {
        return Err(SQLError::Internal(format!(
            "foreign table `{name}` disappeared"
        )));
    }
    let security = catalog.security(&relation).ok_or_else(|| {
        SQLError::Internal(format!(
            "foreign table `{name}` has no loaded security metadata"
        ))
    })?;
    Ok((relation, security))
}

pub fn alter_foreign_table(
    transactions: &dyn ForeignTableAlterTransactions,
    statement: &AlterForeignTableStmt,
) -> Result<(), SQLError> {
    transactions.with_foreign_table_write(Box::new(|context| {
        let Some(canonical) = relation_alteration::foreign_table_alter_target(
            context.names.resolve_relation_kind(&statement.name)?,
            statement,
            &mut |message| {
                context
                    .notices
                    .lock()
                    .push(("NOTICE".into(), message.into()));
            },
        )?
        else {
            return Ok(());
        };
        context.locks.lock_exclusive(&canonical)?;
        match &statement.action {
            AlterForeignTableAction::OwnerTo(owner) => {
                alter_foreign_table_role_owner(context, &canonical, owner)
            }
            AlterForeignTableAction::RenameTo(new_name) => {
                context.access.ensure_owner(&canonical)?;
                let relation = RelationIdentity::from_legacy_name(&canonical).map_err(|error| {
                    SQLError::Internal(format!(
                        "resolve foreign table rename target `{canonical}`: {error}"
                    ))
                })?;
                rename_foreign_table(context, &relation, new_name)
            }
        }
    }))
}

fn alter_foreign_table_role_owner(
    context: &ForeignTableAlterContext<'_>,
    name: &str,
    requested_owner: &str,
) -> Result<(), SQLError> {
    context.writer.prepare_writer()?;
    let (relation, mut security) = bound_foreign_table_security(context.catalog, name)?;
    let current_owner = context.access.ensure_owner(name)?;
    let (new_owner, current_user_is_superuser) =
        role_transfer_target(&context.roles, requested_owner)?;
    if current_owner == new_owner {
        return Ok(());
    }
    if !current_user_is_superuser {
        context
            .roles
            .schemas
            .require_schema_create(&relation.schema, &new_owner)?;
    }
    let table = context.catalog.table(&relation).ok_or_else(|| {
        SQLError::Internal(format!("foreign table `{name}` disappeared before update"))
    })?;
    let sequence_updates =
        table_owned_sequence_owner_updates(context.owned_sequences, table.object_id, &new_owner)?;
    for (sequence, sequence_security) in &sequence_updates {
        context.sequence_publication.persist_security(
            &sequence.qualified_name(),
            sequence,
            sequence_security,
        )?;
    }
    rewrite_acl_owner(&mut security, &new_owner);
    let columns = table
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect::<Vec<_>>();
    validate_table_security_invariants(
        &security, Some(&columns), &context.roles.roles.role_definitions(),
    ).map_err(|error| {
        SQLError::Internal(format!(
            "foreign table `{name}` produced invalid ownership metadata after owner transfer: {error}"
        ))
    })?;
    context.publication.persist_security(&relation, &security)?;
    context
        .publication
        .security_write()
        .insert(relation, security);
    if !sequence_updates.is_empty() {
        let mut registry = context.publication.sequence_security_write();
        for (sequence, sequence_security) in sequence_updates {
            registry.insert(sequence, sequence_security);
        }
    }
    context.changes.catalog_registry_changed();
    Ok(())
}

fn rename_foreign_table(
    context: &ForeignTableAlterContext<'_>,
    relation: &RelationIdentity,
    new_name: &str,
) -> Result<(), SQLError> {
    let target = relation_alteration::relation_rename_target(
        context.names,
        relation,
        new_name,
        "ALTER FOREIGN TABLE RENAME TO",
    )?;
    rewrite_relation_rename_dependents(context.dependencies, relation, &target).map_err(
        |error| {
            SQLError::Internal(format!(
                "rewrite dependencies while renaming foreign table `{}`: {error}",
                relation.qualified_name()
            ))
        },
    )?;
    if context
        .publication
        .persist_rename(relation, &target)
        .map_err(|error| {
            SQLError::Internal(format!(
                "persist foreign table rename `{}` to `{}`: {error}",
                relation.qualified_name(),
                target.qualified_name()
            ))
        })?
        == Some(false)
    {
        return Err(SQLError::Internal(format!(
            "foreign table `{}` disappeared during rename",
            relation.qualified_name()
        )));
    }
    let mut tables = context.publication.tables_write();
    let mut security = context.publication.security_write();
    if tables.contains_key(&target) || security.contains_key(&target) {
        return Err(SQLError::Internal(format!(
            "foreign table rename target `{}` appeared after preflight",
            target.qualified_name()
        )));
    }
    let mut table = tables.remove(relation).ok_or_else(|| {
        SQLError::Internal(format!(
            "foreign table `{}` disappeared during rename",
            relation.qualified_name()
        ))
    })?;
    let table_security = security.remove(relation).ok_or_else(|| {
        SQLError::Internal(format!(
            "foreign table `{}` lost its security metadata during rename",
            relation.qualified_name()
        ))
    })?;
    table.name = target.qualified_name();
    tables.insert(target.clone(), table);
    security.insert(target.clone(), table_security);
    drop(security);
    drop(tables);
    let mut memory_tables = context.publication.memory_tables_write();
    if let Some(rows) = memory_tables.remove(relation) {
        memory_tables.insert(target, rows);
    }
    drop(memory_tables);
    context.changes.catalog_registry_changed();
    Ok(())
}
