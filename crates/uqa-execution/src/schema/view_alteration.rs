//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute view options, owner changes and renames within the caller's catalog transaction.
use super::{
    publication::dependencies::CatalogPublicationChanges,
    relation_alteration::{
        rewrite_relation_rename_dependents, role_transfer_target, RelationAlterLocks,
        RelationRenameDependencies, RoleTransferContext,
    },
};
use crate::catalog::view::{catalog_view_row, StoredView};
use std::{collections::BTreeMap, ops::DerefMut};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterViewAction, AlterViewKind, AlterViewStmt, RelationPersistence},
    catalog::security::table::{rewrite_acl_owner, validate_table_security_invariants},
    schema::relation_alteration::{self, RelationAlterNames},
    semantics::view_rewrite::{context::ViewRewriteContext, validate_view_definition_check_option},
    SQLError,
};
use uqa_storage::{StorageBackendResult, ViewRow};

pub type ViewRegistryWrite<'a> =
    Box<dyn DerefMut<Target = BTreeMap<RelationIdentity, StoredView>> + 'a>;

pub trait ViewAlterCatalog {
    fn view(&self, relation: &RelationIdentity) -> Option<StoredView>;
    fn persistence(&self, relation: &RelationIdentity) -> Option<RelationPersistence>;
}
pub trait ViewAlterAccess {
    fn ensure_owner(&self, name: &str, view: &StoredView) -> Result<String, SQLError>;
}
pub trait ViewAlterPublication {
    fn has_catalog(&self) -> bool;
    fn save_view(&self, row: &ViewRow) -> StorageBackendResult<()>;
    fn persist_rename(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<Option<bool>>;
    fn views_write(&self) -> ViewRegistryWrite<'_>;
}

pub struct ViewAlterContext<'a> {
    pub names: &'a dyn RelationAlterNames,
    pub catalog: &'a dyn ViewAlterCatalog,
    pub access: &'a dyn ViewAlterAccess,
    pub locks: &'a dyn RelationAlterLocks,
    pub roles: RoleTransferContext<'a>,
    pub dependencies: &'a dyn RelationRenameDependencies,
    pub publication: &'a dyn ViewAlterPublication,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub rewrite: ViewRewriteContext<'a>,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}
pub type ViewAlterWrite<'a> = Box<dyn FnOnce(&ViewAlterContext<'_>) -> Result<(), SQLError> + 'a>;
pub trait ViewAlterTransactions {
    fn with_view_write(&self, write: ViewAlterWrite<'_>) -> Result<(), SQLError>;
}

pub fn alter_view(
    transactions: &dyn ViewAlterTransactions,
    statement: &AlterViewStmt,
) -> Result<(), SQLError> {
    transactions.with_view_write(Box::new(|context| execute_alter_view(context, statement)))
}

fn execute_alter_view(
    context: &ViewAlterContext<'_>,
    statement: &AlterViewStmt,
) -> Result<(), SQLError> {
    let Some(target) = relation_alteration::view_alter_target(
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
    let relation = &target.relation;
    let canonical = &target.canonical;
    let expected_kind = target.kind;
    let mut view = context
        .catalog
        .view(relation)
        .ok_or_else(|| SQLError::Internal(format!("{expected_kind} `{canonical}` disappeared")))?;
    context.access.ensure_owner(canonical, &view)?;
    context.locks.lock_exclusive(canonical)?;
    match &statement.action {
        AlterViewAction::Set(changes) => {
            relation_alteration::set_view_options(&mut view.options, changes);
        }
        AlterViewAction::Reset(names) => {
            relation_alteration::reset_view_options(&mut view.options, names);
        }
        AlterViewAction::OwnerTo(owner) => {
            alter_view_role_owner(context, canonical, &mut view, owner)?;
        }
        AlterViewAction::RenameTo(new_name) => {
            return rename_view(context, relation, new_name, expected_kind);
        }
    }
    if statement.kind == AlterViewKind::View {
        validate_view_definition_check_option(
            context.rewrite,
            canonical,
            &view.rewrite_definition(),
        )?;
    }
    if view.persistence != RelationPersistence::Temporary && context.publication.has_catalog() {
        context
            .publication
            .save_view(&catalog_view_row(relation, &view).map_err(|error| {
                SQLError::Internal(format!(
                    "serialize altered {expected_kind} `{canonical}`: {error}"
                ))
            })?)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "persist altered {expected_kind} `{canonical}`: {error}"
                ))
            })?;
    }
    context
        .publication
        .views_write()
        .insert(target.relation, view);
    context.changes.catalog_registry_changed();
    Ok(())
}

fn alter_view_role_owner(
    context: &ViewAlterContext<'_>,
    canonical_name: &str,
    view: &mut StoredView,
    requested_owner: &str,
) -> Result<(), SQLError> {
    let current_owner = context.access.ensure_owner(canonical_name, view)?;
    let (new_owner, current_user_is_superuser) =
        role_transfer_target(&context.roles, requested_owner)?;
    if current_owner == new_owner {
        return Ok(());
    }
    let relation = RelationIdentity::from_legacy_name(canonical_name).map_err(|error| {
        SQLError::Internal(format!(
            "resolve view owner target `{canonical_name}`: {error}"
        ))
    })?;
    if !current_user_is_superuser {
        context
            .roles
            .schemas
            .require_schema_create(&relation.schema, &new_owner)?;
    }
    let mut security = view.security();
    rewrite_acl_owner(&mut security, &new_owner);
    let output_columns = view.output_columns.as_deref().ok_or_else(|| {
        SQLError::Internal(format!(
            "loaded view `{canonical_name}` has no durable public column metadata"
        ))
    })?;
    validate_table_security_invariants(
        &security, Some(output_columns), &context.roles.roles.role_definitions(),
    ).map_err(|error| {
        SQLError::Internal(format!(
            "view `{canonical_name}` produced invalid privilege metadata after owner transfer: {error}"
        ))
    })?;
    view.set_security(security);
    Ok(())
}

fn rename_view(
    context: &ViewAlterContext<'_>,
    relation: &RelationIdentity,
    new_name: &str,
    expected_kind: &str,
) -> Result<(), SQLError> {
    let target = relation_alteration::relation_rename_target(
        context.names,
        relation,
        new_name,
        "ALTER VIEW RENAME TO",
    )?;
    rewrite_relation_rename_dependents(context.dependencies, relation, &target).map_err(
        |error| {
            SQLError::Internal(format!(
                "rewrite dependencies while renaming {expected_kind} `{}`: {error}",
                relation.qualified_name()
            ))
        },
    )?;
    let persistent = context
        .catalog
        .persistence(relation)
        .is_some_and(|persistence| persistence != RelationPersistence::Temporary);
    if persistent
        && context
            .publication
            .persist_rename(relation, &target)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "persist {expected_kind} rename `{}` to `{}`: {error}",
                    relation.qualified_name(),
                    target.qualified_name()
                ))
            })?
            == Some(false)
    {
        return Err(SQLError::Internal(format!(
            "{expected_kind} `{}` disappeared during rename",
            relation.qualified_name()
        )));
    }
    let mut views = context.publication.views_write();
    if views.contains_key(&target) {
        return Err(SQLError::Internal(format!(
            "{expected_kind} rename target `{}` appeared after preflight",
            target.qualified_name()
        )));
    }
    let view = views.remove(relation).ok_or_else(|| {
        SQLError::Internal(format!(
            "{expected_kind} `{}` disappeared during rename",
            relation.qualified_name()
        ))
    })?;
    views.insert(target, view);
    drop(views);
    context.changes.catalog_registry_changed();
    Ok(())
}
