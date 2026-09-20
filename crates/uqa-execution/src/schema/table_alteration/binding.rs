//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain the requested ALTER TABLE name until relation locking has selected its current target.

use crate::{
    catalog::security::table_inquiry::TablePrivilegeContext,
    row_locks::binding::{
        bind_relation_with_mode, RelationBinding, RelationDefinitionSession, RelationLockCatalog,
    },
    schema::{
        namespaces::relations::RelationCreationContext,
        relation_alteration::validate_relation_alter_authority,
    },
};
use uqa_sql::{
    ast::{AlterTableAction, AlterTableStmt},
    schema::{
        relation_alteration::RelationAlterNames,
        table_alteration::{
            syntax::table_alter_lock_mode,
            targets::{self, BoundTableAlteration},
        },
    },
    SQLError,
};

pub struct TableAlterBindingContext<'a> {
    pub names: &'a dyn RelationAlterNames,
    pub catalog: &'a dyn RelationLockCatalog,
    pub authority: TablePrivilegeContext<'a>,
    pub creation: RelationCreationContext<'a>,
    pub locks: &'a dyn RelationDefinitionSession,
    pub notices: &'a parking_lot::Mutex<Vec<(String, String)>>,
}

pub fn bind_table_alteration(
    context: &TableAlterBindingContext<'_>,
    statement: AlterTableStmt,
) -> Result<Option<BoundTableAlteration>, SQLError> {
    bind_alteration(context, statement, false)
}

pub(super) fn bind_alteration(
    context: &TableAlterBindingContext<'_>,
    statement: AlterTableStmt,
    index_rename: bool,
) -> Result<Option<BoundTableAlteration>, SQLError> {
    let binding = bind_relation_with_mode(
        context.locks,
        |binding: &RelationBinding<uqa_sql::schema::relation_alteration::RelationAlterTarget>| {
            if index_rename && binding.value.kind == "index" {
                crate::row_locks::RelationLockMode::ShareUpdateExclusive
            } else {
                table_alter_lock_mode(&statement).into()
            }
        },
        false,
        || {
            let Some(target) = targets::table_alter_target(
                context.names.resolve_relation_kind(&statement.table)?,
                &statement,
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
            Ok(Some(RelationBinding {
                object_id: context.catalog.relation_object_id(&target.canonical)?,
                name: target.canonical.clone(),
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
                matches!(
                    statement.actions.as_slice(),
                    [AlterTableAction::RenameTable { .. }]
                ),
            )
        },
    )?;
    binding
        .map(|binding| targets::bind_table_alteration(binding.value, statement))
        .transpose()
}
