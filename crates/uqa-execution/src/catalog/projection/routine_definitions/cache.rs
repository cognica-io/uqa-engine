//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain routine addresses, borrowing user definitions until the selected routine is needed.

use super::{BuiltinRoutineCatalogEntry, CatalogReadView, SQLError, SQLUserFunction};
use crate::catalog::cache::OrderedOidLookup;
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

#[derive(Default)]
pub(in crate::catalog) struct RoutineDefinitions {
    by_oid: OnceLock<OrderedOidLookup<(String, usize)>>,
}

pub(super) fn user_routine_by_oid(
    catalog: &CatalogReadView,
    oid: i64,
) -> Result<Option<&Arc<SQLUserFunction>>, SQLError> {
    let lookup = catalog.routine_definitions.by_oid.get_or_init(|| {
        OrderedOidLookup::build(
            catalog
                .snapshot()
                .definitions
                .sql_user_functions
                .iter()
                .flat_map(|(name, overloads)| {
                    overloads
                        .iter()
                        .enumerate()
                        .map(move |(position, function)| {
                            #[cfg(test)]
                            ROUTINE_ADDRESSES.set(ROUTINE_ADDRESSES.get() + 1);
                            super::super::pg_proc::user_routine_catalog_oid(function)
                                .map(|oid| (oid, (name.clone(), position)))
                        })
                }),
        )
    });
    Ok(lookup.get(oid)?.map(|(name, position)| {
        &catalog.snapshot().definitions.sql_user_functions[name][*position]
    }))
}

pub(super) fn builtin_routine_by_oid(oid: i64) -> Option<BuiltinRoutineCatalogEntry> {
    static BUILTINS: OnceLock<BTreeMap<i64, BuiltinRoutineCatalogEntry>> = OnceLock::new();
    BUILTINS
        .get_or_init(|| {
            let mut by_oid = BTreeMap::new();
            for entry in super::super::builtin_routines::PG18_BUILTIN_ROUTINE_GROUPS
                .iter()
                .flat_map(|group| group.iter())
                .copied()
                .chain(super::super::builtin_routines::native_foreign_handlers())
            {
                by_oid.entry(entry.oid).or_insert(entry);
            }
            by_oid
        })
        .get(&oid)
        .copied()
}

#[cfg(test)]
thread_local! {
    static ROUTINE_ADDRESSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
