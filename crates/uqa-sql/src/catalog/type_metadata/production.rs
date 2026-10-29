//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine identity lookup retains admitted spelling and parsed type workspace only while observing its OID.

use super::{pg_type_oid, stable_oid, ColumnType, ROUTINE_PSEUDO_TYPES};
use crate::SQLError;
use uqa_core::memory::ProductionControl;

pub fn routine_type_oid_with_control(
    type_name: &str,
    control: &ProductionControl<'_>,
) -> Result<i64, SQLError> {
    let canonical =
        crate::type_resolution::canonical_routine_type_name_with_control(type_name, control)?;
    if let Some((_, oid)) = ROUTINE_PSEUDO_TYPES
        .iter()
        .find(|(name, _)| *name == canonical.as_str())
    {
        return Ok(*oid);
    }
    match ColumnType::from_sql_name_with_control(&canonical, control) {
        Ok(ty) => Ok(pg_type_oid(&ty)),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(stable_oid("type", &canonical)),
    }
}
