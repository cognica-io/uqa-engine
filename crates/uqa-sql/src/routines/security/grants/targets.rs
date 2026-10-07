//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine privilege target selection by immutable `pg_proc` addresses.

pub use crate::routines::lifecycle::names::RoutineCatalogIdentity as RoutinePrivilegeIdentity;
use crate::{
    ast::AlterRoutineKind,
    routines::lifecycle::{
        alter_routine_kind_name, ambiguous_routine_error,
        names::{routine_lookup_keys_with_builtins, RoutineNameCatalog},
        routine_signature_display, wrong_routine_kind_error,
    },
    SQLError,
};
use std::collections::BTreeSet;

pub fn in_schemas(
    catalog: &dyn RoutineNameCatalog,
    identities: &[RoutinePrivilegeIdentity],
    schemas: &[String],
    kind: AlterRoutineKind,
) -> Result<Vec<usize>, SQLError> {
    let mut result = Vec::new();
    for schema in schemas {
        let security = catalog
            .schema_security(schema)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "3F000".into(),
                message: format!("schema \"{schema}\" does not exist"),
            })?;
        catalog.require_schema_usage(schema, &catalog.current_role())?;
        let namespace = security.namespace_oid(schema);
        let mut selected = identities
            .iter()
            .enumerate()
            .filter(|(_, identity)| {
                super::kind_matches(kind, identity.kind)
                    && catalog
                        .schema_security(&identity.relation.schema)
                        .is_some_and(|security| {
                            security.namespace_oid(&identity.relation.schema) == namespace
                        })
            })
            .map(|(index, identity)| (identity.oid, index))
            .collect::<Vec<_>>();
        selected.sort_by_key(|(oid, _)| *oid);
        result.extend(selected.into_iter().map(|(_, index)| index));
    }
    Ok(result)
}

pub fn named(
    catalog: &dyn RoutineNameCatalog,
    identities: &[RoutinePrivilegeIdentity],
    name: &str,
    requested_types: Option<&[i64]>,
    display_types: Option<&[String]>,
    kind: AlterRoutineKind,
) -> Result<usize, SQLError> {
    let keys = routine_lookup_keys_with_builtins(catalog, name)?;
    let kind_name = alter_routine_kind_name(kind);
    let mut visible = BTreeSet::new();
    let mut selected = Vec::new();
    for key in keys {
        for (index, identity) in identities
            .iter()
            .enumerate()
            .filter(|(_, identity)| identity.relation.qualified_name() == key)
        {
            if let Some(types) = requested_types {
                if identity.argument_types != types {
                    continue;
                }
                if !super::kind_matches(kind, identity.kind) {
                    return Err(wrong_routine_kind_error(
                        &routine_signature_display(
                            catalog,
                            name,
                            display_types.unwrap_or_default(),
                        ),
                        kind_name,
                    ));
                }
                return Ok(index);
            }
            if visible.insert(identity.argument_types.clone())
                && super::kind_matches(kind, identity.kind)
            {
                selected.push(index);
            }
        }
    }
    match selected.as_slice() {
        [index] => Ok(*index),
        [] => {
            let missing_kind = if kind == AlterRoutineKind::Procedure {
                "procedure"
            } else {
                "function"
            };
            let message = display_types.map_or_else(
                || format!("could not find a {missing_kind} named \"{name}\""),
                |types| {
                    format!(
                        "{missing_kind} {} does not exist",
                        routine_signature_display(catalog, name, types)
                    )
                },
            );
            Err(SQLError::Routine {
                sqlstate: "42883".into(),
                message,
            })
        }
        _ => Err(ambiguous_routine_error(kind_name, name)),
    }
}
