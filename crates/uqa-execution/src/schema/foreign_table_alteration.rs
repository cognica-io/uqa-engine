//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-table ALTER execution and publication through retained catalog write guards.
use super::{
    publication::dependencies::CatalogPublicationChanges,
    relation_alteration::{
        rewrite_relation_rename_dependents, validate_relation_alter_authority,
        RelationRenameDependencies, RoleTransferContext,
    },
    sequences::role_ownership::{
        table_owned_sequence_owner_updates, OwnedSequenceSecurityCatalog,
        OwnedSequenceSecurityWrite, SequenceSecurityPublication,
    },
};
use crate::catalog::security::roles::dependencies::{prepare_role_owner, RoleDependencyCandidate};
use crate::catalog::{foreign::StoredForeignTable, security::BoundTableSecurity};
use crate::row_locks::{
    binding::{bind_relation, RelationBinding, RelationDefinitionSession},
    RelationLockMode,
};
use std::{collections::BTreeMap, ops::DerefMut};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterForeignTableAction, AlterForeignTableStmt},
    catalog::security::ownership::OwnerChangeAuthority,
    catalog::security::table::{rewrite_acl_owner, validate_table_security_invariants},
    schema::relation_alteration::{self, RelationAlterNames},
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub type ForeignTableRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, StoredForeignTable>> + 'a>;
pub type ForeignSecurityRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, BoundTableSecurity>> + 'a>;
pub type ForeignMemoryRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, Vec<uqa_fdw::Row>>> + 'a>;

pub trait ForeignTableAlterCatalog {
    fn contains_table(&self, relation: &RelationIdentity) -> bool;
    fn security(&self, relation: &RelationIdentity) -> Option<BoundTableSecurity>;
    fn table(&self, relation: &RelationIdentity) -> Option<StoredForeignTable>;
}
pub trait ForeignTableAlterAccess {
    fn ensure_owner(&self, name: &str) -> Result<String, SQLError>;
}
pub trait ForeignTableOwnerWriter {
    fn prepare_writer(&self) -> Result<(), SQLError>;
}
pub trait ForeignTableAlterPublication {
    fn persist_definition(
        &self,
        relation: &RelationIdentity,
        table: &StoredForeignTable,
    ) -> StorageBackendResult<()>;
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
        security: &BoundTableSecurity,
    ) -> Result<(), SQLError>;
}
pub struct ForeignTableAlterContext<'a> {
    pub schema_moves: super::relation_alteration::relocation::RelationSchemaContext<'a>,
    pub names: &'a dyn RelationAlterNames,
    pub catalog: &'a dyn ForeignTableAlterCatalog,
    pub authority: crate::catalog::security::table_inquiry::TablePrivilegeContext<'a>,
    pub creation: super::namespaces::relations::RelationCreationContext<'a>,
    pub locks: &'a dyn RelationDefinitionSession,
    pub writer: &'a dyn ForeignTableOwnerWriter,
    pub roles: RoleTransferContext<'a>,
    pub dependencies: &'a dyn RelationRenameDependencies,
    pub owned_sequences: &'a dyn OwnedSequenceSecurityCatalog,
    pub sequence_publication: &'a dyn SequenceSecurityPublication,
    pub publication: &'a dyn ForeignTableAlterPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub notices: &'a crate::query::NoticeQueue,
}
pub type ForeignTableAlterWrite<'a> =
    Box<dyn FnOnce(&ForeignTableAlterContext<'_>) -> Result<(), SQLError> + 'a>;
pub trait ForeignTableAlterTransactions {
    fn with_foreign_table_write(&self, write: ForeignTableAlterWrite<'_>) -> Result<(), SQLError>;
}

pub fn bound_foreign_table_security(
    catalog: &dyn ForeignTableAlterCatalog,
    name: &str,
) -> Result<(RelationIdentity, BoundTableSecurity), SQLError> {
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
        let Some(binding) = bind_relation(
            context.locks,
            RelationLockMode::AccessExclusive,
            false,
            || {
                let Some(target) = relation_alteration::foreign_table_alter_target(
                    context.names.resolve_relation_kind(&statement.name)?,
                    statement,
                    &mut |message| {
                        context.notices.push(uqa_sql::SQLNotice::notice(message));
                    },
                )?
                else {
                    return Ok(None);
                };
                Ok(Some(RelationBinding {
                    name: target.canonical.clone(),
                    object_id: context
                        .catalog
                        .table(&target.relation)
                        .map(|table| table.object_id),
                    value: target,
                }))
            },
            |binding| {
                let target = &binding.value;
                validate_relation_alter_authority(
                    &context.authority,
                    &context.creation,
                    &target.relation,
                    target.kind,
                    matches!(statement.action, AlterForeignTableAction::RenameTo(_)),
                )?;
                target.require_kind("foreign table")
            },
        )?
        else {
            return Ok(());
        };
        let canonical = binding.name;
        match &statement.action {
            AlterForeignTableAction::OwnerTo(owner) => {
                alter_foreign_table_role_owner(context, &canonical, owner)
            }
            AlterForeignTableAction::RenameTo(new_name) => {
                context.locks.prepare_definition_write()?;
                rename_foreign_table(context, &binding.value.relation, new_name)
            }
            AlterForeignTableAction::SetSchema(schema) => {
                let relation = &binding.value.relation;
                let current = context
                    .catalog
                    .table(relation)
                    .ok_or_else(|| SQLError::Internal("moved foreign table disappeared".into()))?;
                if let Some(target) =
                    context
                        .schema_moves
                        .target(relation, schema, current.persistence)?
                {
                    let relocation =
                        context
                            .schema_moves
                            .prepare(relation, &target, current.object_id)?;
                    rename_foreign_table_to(context, relation, target.clone())?;
                    relocation.publish(&context.schema_moves)?;
                }
                Ok(())
            }
        }
    }))
}

fn alter_foreign_table_role_owner(
    context: &ForeignTableAlterContext<'_>,
    name: &str,
    requested_owner: &uqa_sql::ast::RoleSpecification,
) -> Result<(), SQLError> {
    let owner = context.roles.bind(requested_owner)?;
    let current_user = context.roles.session.current_role();
    let RoleDependencyCandidate {
        roles,
        memberships,
        value,
        ..
    } = prepare_role_owner(
        context.roles.lock_context(),
        &owner,
        || context.writer.prepare_writer(),
        |roles, memberships, new_owner| {
            let (relation, bound) = bound_foreign_table_security(context.catalog, name)?;
            let mut security = bound.resolve(roles).map_err(SQLError::Internal)?;
            if security.role_owner == new_owner {
                return Ok(None);
            }
            let authority = OwnerChangeAuthority {
                roles,
                memberships,
                current_user: &current_user,
                new_owner,
            };
            authority.require_owner_change(
                &security.role_owner,
                "foreign table",
                &relation.name,
            )?;
            authority.require_schema_create(context.roles.schemas, &relation.schema)?;
            let table = context.catalog.table(&relation).ok_or_else(|| {
                SQLError::Internal(format!("foreign table `{name}` disappeared before update"))
            })?;
            rewrite_acl_owner(&mut security, new_owner);
            let columns = table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            validate_table_security_invariants(&security, Some(&columns), roles)
                .map_err(|error| SQLError::Internal(format!("foreign table `{name}` produced invalid ownership metadata after owner transfer: {error}")))?;
            Ok(Some((
                relation,
                table.object_id,
                BoundTableSecurity::bind(&security, roles).map_err(SQLError::Internal)?,
            )))
        },
    )?;
    let Some((relation, object_id, security)) = value else {
        return Ok(());
    };
    let sequence_updates = table_owned_sequence_owner_updates(
        context.owned_sequences,
        object_id,
        owner.require_name(&roles)?,
        &roles,
    )?;
    for (sequence, sequence_security) in &sequence_updates {
        context.sequence_publication.persist_security(
            &sequence.qualified_name(),
            sequence,
            sequence_security,
        )?;
    }
    context.publication.persist_security(&relation, &security)?;
    context
        .publication
        .security_write()
        .insert(relation.clone(), security);
    if !sequence_updates.is_empty() {
        let mut registry = context.publication.sequence_security_write();
        for (sequence, sequence_security) in sequence_updates {
            registry.insert(sequence, sequence_security);
        }
    }
    drop(memberships);
    drop(roles);
    context.changes.catalog_registry_changed();
    context.changes.prepared_relation_changed(&relation);
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
    rename_foreign_table_to(context, relation, target)
}

/// Move a foreign table into `schema` under its own name while its schema is renamed.
pub fn relocate_foreign_table(
    context: &ForeignTableAlterContext<'_>,
    relation: &RelationIdentity,
    schema: &str,
) -> Result<(), SQLError> {
    let target = RelationIdentity::new(schema, &relation.name);
    if context
        .names
        .relation_kind_at(&target.qualified_name())
        .map_err(|error| {
            SQLError::Internal(format!(
                "check schema rename target `{}`: {error}",
                target.qualified_name()
            ))
        })?
        .is_some()
    {
        return Err(SQLError::Internal(format!(
            "schema rename target `{}` already exists",
            target.qualified_name()
        )));
    }
    rename_foreign_table_to(context, relation, target)
}

fn rename_foreign_table_to(
    context: &ForeignTableAlterContext<'_>,
    relation: &RelationIdentity,
    target: RelationIdentity,
) -> Result<(), SQLError> {
    let current = context
        .catalog
        .table(relation)
        .ok_or_else(|| SQLError::Internal("renamed foreign table disappeared".into()))?;
    let array_name = super::types::relation_arrays::rename(
        &context.creation,
        relation,
        &target,
        current.row_type_array_name.as_deref(),
        current.relation_oids().array_type,
    )?;
    rewrite_relation_rename_dependents(context.dependencies, relation, &target).map_err(
        |error| {
            SQLError::Internal(format!(
                "rewrite dependencies while renaming foreign table `{}`: {error}",
                relation.qualified_name()
            ))
        },
    )?;
    if current.persistence != uqa_sql::ast::RelationPersistence::Temporary
        && context
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
    table.row_type_array_name = array_name;
    tables.insert(target.clone(), table);
    security.insert(target.clone(), table_security);
    drop(security);
    drop(tables);
    let renamed = context
        .catalog
        .table(&target)
        .ok_or_else(|| SQLError::Internal("renamed foreign table disappeared".into()))?;
    context
        .publication
        .persist_definition(&target, &renamed)
        .map_err(|error| uqa_sql::catalog::errors::storage_error("foreign array rename", &error))?;
    let mut memory_tables = context.publication.memory_tables_write();
    if let Some(rows) = memory_tables.remove(relation) {
        memory_tables.insert(target.clone(), rows);
    }
    drop(memory_tables);
    context.changes.catalog_registry_changed();
    context.changes.prepared_relation_changed(&target);
    Ok(())
}
