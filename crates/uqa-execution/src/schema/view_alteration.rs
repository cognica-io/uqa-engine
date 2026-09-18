//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute view options, owner changes and renames within the caller's catalog transaction.
use super::{
    publication::dependencies::CatalogPublicationChanges,
    relation_alteration::{
        rewrite_relation_rename_dependents, RelationRenameDependencies, RoleTransferContext,
    },
};
use crate::catalog::security::roles::dependencies::{prepare_role_owner, RoleDependencyCandidate};
use crate::catalog::view::ViewPublication;
use crate::catalog::view::{catalog_view_row, StoredView};
use crate::row_locks::binding::{bind_relation, RelationBinding, RelationDefinitionSession};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{AlterViewAction, AlterViewKind, AlterViewStmt, RelationPersistence},
    catalog::security::ownership::OwnerChangeAuthority,
    catalog::security::table::{rewrite_acl_owner, validate_table_security_invariants},
    schema::relation_alteration::{self, RelationAlterNames},
    semantics::view_rewrite::{context::ViewRewriteContext, validate_view_definition_check_option},
    SQLError,
};
use uqa_storage::StorageBackendResult;

pub use crate::catalog::view::ViewRegistryWrite;

pub trait ViewAlterCatalog {
    fn view(&self, relation: &RelationIdentity) -> Option<StoredView>;
    fn persistence(&self, relation: &RelationIdentity) -> Option<RelationPersistence>;
}
pub trait ViewAlterAccess {
    fn ensure_owner(&self, name: &str, view: &StoredView) -> Result<String, SQLError>;
}
pub trait ViewAlterPublication: ViewPublication {
    fn persist_rename(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<Option<bool>>;
}

pub struct ViewAlterContext<'a> {
    pub names: &'a dyn RelationAlterNames,
    pub catalog: &'a dyn ViewAlterCatalog,
    pub access: &'a dyn ViewAlterAccess,
    pub locks: &'a dyn RelationDefinitionSession,
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
    let Some(binding) = bind_relation(
        context.locks,
        relation_alteration::view_alter_lock_mode(statement).into(),
        false,
        || {
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
                return Ok(None);
            };
            let view = context.catalog.view(&target.relation).ok_or_else(|| {
                SQLError::Internal(format!(
                    "{} `{}` disappeared",
                    target.kind, target.canonical
                ))
            })?;
            Ok(Some(RelationBinding {
                name: target.canonical.clone(),
                object_id: Some(view.object_id),
                value: (target, view),
            }))
        },
        |binding| {
            context
                .access
                .ensure_owner(&binding.name, &binding.value.1)
                .map(|_| ())
        },
    )?
    else {
        return Ok(());
    };
    let (target, mut view) = binding.value;
    let relation = &target.relation;
    let canonical = &target.canonical;
    let expected_kind = target.kind;
    if let AlterViewAction::OwnerTo(owner) = &statement.action {
        return alter_view_role_owner(context, relation, canonical, expected_kind, owner);
    }
    context.locks.prepare_definition_write()?;
    match &statement.action {
        AlterViewAction::Set(changes) => {
            relation_alteration::set_view_options(&mut view.options, changes);
        }
        AlterViewAction::Reset(names) => {
            relation_alteration::reset_view_options(&mut view.options, names);
        }
        AlterViewAction::OwnerTo(_) => unreachable!("owner changes prepare their role dependency"),
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
    publish_altered_view(context, relation, canonical, expected_kind, view)?;
    context.changes.catalog_registry_changed();
    Ok(())
}

fn publish_altered_view(
    context: &ViewAlterContext<'_>,
    relation: &RelationIdentity,
    canonical: &str,
    expected_kind: &str,
    view: StoredView,
) -> Result<(), SQLError> {
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
        .insert(relation.clone(), view);
    Ok(())
}

fn alter_view_role_owner(
    context: &ViewAlterContext<'_>,
    relation: &RelationIdentity,
    canonical_name: &str,
    kind: &str,
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
        || context.locks.prepare_definition_write(),
        |roles, memberships| {
            let mut view = context.catalog.view(relation).ok_or_else(|| {
                SQLError::Internal(format!(
                    "{kind} `{canonical_name}` disappeared during owner change"
                ))
            })?;
            if view.security.role_owner == owner.identity() {
                return Ok(None);
            }
            let authority = OwnerChangeAuthority {
                roles,
                memberships,
                current_user: &current_user,
                new_owner: &owner.name,
            };
            let mut security = view.security.resolve(roles).map_err(SQLError::Internal)?;
            authority.require_owner_change(&security.role_owner, kind, &relation.name)?;
            authority.require_schema_create(context.roles.schemas, &relation.schema)?;
            rewrite_acl_owner(&mut security, &owner.name);
            let output_columns = view.output_columns.as_deref().ok_or_else(|| {
                SQLError::Internal(format!(
                    "loaded view `{canonical_name}` has no durable public column metadata"
                ))
            })?;
            validate_table_security_invariants(&security, Some(output_columns), roles)
                .map_err(|error| SQLError::Internal(format!("view `{canonical_name}` produced invalid privilege metadata after owner transfer: {error}")))?;
            view.set_security(
                uqa_sql::catalog::security::BoundTableSecurity::bind(&security, roles)
                    .map_err(SQLError::Internal)?,
            );
            Ok(Some(view))
        },
    )?;
    let Some(view) = value else {
        return Ok(());
    };
    publish_altered_view(context, relation, canonical_name, kind, view)?;
    drop(memberships);
    drop(roles);
    context.changes.catalog_registry_changed();
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
