//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RoutinePrivilegeContext;
use std::collections::BTreeSet;
use uqa_core::catalog_role::RoleIdentity;
use uqa_sql::{catalog::security::drop_owned, SQLError};

pub(crate) fn remove_role_grants(
    context: &RoutinePrivilegeContext<'_>,
    oid: u32,
    roles: &BTreeSet<RoleIdentity>,
) -> Result<(), SQLError> {
    context.catalog.writer.prepare_writer()?;
    let mut builtins = context.builtin_security.builtin_routine_securities_write();
    if let Some(entry) = builtins.get(&oid) {
        let mut acl = Some(entry.execute_acl.clone());
        drop_owned::object(RoleIdentity::BOOTSTRAP, &mut acl, roles)?;
        let acl = acl.expect("existing builtin ACL");
        if acl != entry.execute_acl {
            let update =
                crate::catalog::security::builtin_routines::BuiltinRoutinePrivilegeUpdate::new(
                    oid, acl,
                )
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            update
                .persist(context.storage)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            builtins.insert(oid, update.entry)?;
            context.catalog.changes.catalog_registry_changed();
        }
        return Ok(());
    }
    drop(builtins);
    let mut registry = context.catalog.registry.routines_write();
    let mut selected = None;
    for (name, routines) in &**registry {
        for (index, routine) in routines.iter().enumerate() {
            if crate::catalog::projection::user_routine_catalog_oid(routine)? == i64::from(oid) {
                selected = Some((name.clone(), index));
                break;
            }
        }
        if selected.is_some() {
            break;
        }
    }
    let Some((name, index)) = selected else {
        return Ok(());
    };
    let existing = &registry[&name][index];
    let mut definition = existing.def.clone();
    let owner = uqa_sql::routines::security::bound_routine_owner(&definition)?;
    drop_owned::object(owner, &mut definition.execute_acl, roles)?;
    if definition.execute_acl == existing.def.execute_acl {
        return Ok(());
    }
    let replacement =
        crate::routines::catalog::revision::replacement(definition, existing.body.clone())?;
    let mut next = registry.clone();
    next.edit(&name, |overloads| {
        overloads[index] = replacement;
        Ok(())
    })?;
    context
        .catalog
        .publication
        .persist_routine_definitions(&next)?;
    crate::routines::catalog::publication::record_changes(
        context.catalog.changes,
        &registry,
        &next,
    );
    **registry = next;
    drop(registry);
    context.catalog.changes.catalog_registry_changed();
    Ok(())
}
