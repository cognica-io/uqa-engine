//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected builtin OIDs borrow the same declarations used to publish catalog rows.

use super::{builtins, polymorphic, routine_internal, special};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    catalog::type_metadata::{builtin_type_name, pg_type_array_oid, pg_type_oid},
    SQLError,
};

pub(in crate::catalog::projection) fn builtin_type_identity_by_name(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<(i64, i64)>, SQLError> {
    for (ty, _, _, kind) in builtins::CATALOG_TYPES {
        control.check()?;
        let local = builtin_type_name(ty).expect("catalog builtin type name");
        let array = pg_type_array_oid(ty);
        if name == local {
            return Ok(Some((pg_type_oid(ty), array)));
        }
        if matches!(*kind, "b" | "r" | "m") && array != 0 && name.strip_prefix('_') == Some(local) {
            return Ok(Some((array, 0)));
        }
    }
    for entry in polymorphic::metadata()
        .chain(routine_internal::metadata())
        .chain(special::metadata())
    {
        control.check()?;
        if entry.name == name {
            return Ok(Some((entry.oid, entry.array_oid)));
        }
    }
    for (local, [oid, array]) in [
        ("pg_language", super::super::languages::type_oids()),
        ("pg_extension", super::super::extensions::type_oids()),
    ] {
        if name == local {
            return Ok(Some((oid, array)));
        }
        if name.strip_prefix('_') == Some(local) {
            return Ok(Some((array, 0)));
        }
    }
    for entry in uqa_sql::catalog::foreign_relations::FOREIGN_CATALOGS {
        control.check()?;
        if name == entry.relation.name() {
            return Ok(Some((
                i64::from(entry.row_type),
                i64::from(entry.array_type),
            )));
        }
        if name.strip_prefix('_') == Some(entry.relation.name()) {
            return Ok(Some((i64::from(entry.array_type), 0)));
        }
    }
    Ok(None)
}
