//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join each original routine declaration with the restored type owner before activation.

mod input;
pub(super) use input::{BorrowedTypes, Cursor};
pub use input::{DeclarationTypeLookup, VariadicLookup};

use crate::catalog::CatalogReadSnapshot;
use uqa_core::memory::ProductionControl;
use uqa_sql::{routines::declaration::RoutineTypeCatalog, SQLError};

pub fn validate(
    snapshot: &CatalogReadSnapshot,
    types: &dyn RoutineTypeCatalog,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    for routine in snapshot.definitions.sql_user_functions.values().flatten() {
        uqa_sql::routines::declaration::validate_captured_declaration(
            &routine.def,
            types,
            control,
        )?;
    }
    Ok(control.check()?)
}

#[cfg(test)]
mod tests;
