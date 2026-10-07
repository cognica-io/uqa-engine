//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The immutable identities exposed by `pg_proc`, without constructing catalog rows.

use super::PG18_BUILTIN_ROUTINE_GROUPS;

pub use uqa_sql::catalog::security::builtin_routines::BuiltinRoutineIdentity;

pub fn builtin_routine_identities() -> impl Iterator<Item = BuiltinRoutineIdentity> {
    PG18_BUILTIN_ROUTINE_GROUPS
        .iter()
        .flat_map(|group| group.iter())
        .copied()
        .chain(super::native_foreign_handlers())
        .map(|routine| BuiltinRoutineIdentity {
            oid: u32::try_from(routine.oid).expect("builtin catalog OID"),
            name: routine.name,
            kind: routine.kind.chars().next().expect("builtin routine kind"),
            argument_types: routine.argument_types,
        })
        .chain(
            uqa_sql::registry::registered_names()
                .into_iter()
                .map(|name| BuiltinRoutineIdentity {
                    oid: u32::try_from(uqa_sql::catalog::oids::stable_oid("proc", name))
                        .expect("registered routine OID"),
                    name,
                    kind: 'f',
                    argument_types: &[],
                }),
        )
}

pub fn builtin_routine_identity(oid: u32) -> Option<BuiltinRoutineIdentity> {
    builtin_routine_identities().find(|routine| routine.oid == oid)
}
