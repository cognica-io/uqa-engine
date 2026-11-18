//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join foreign wrapper callbacks to the selected private SQL routine catalog.

use crate::catalog::CatalogReadSnapshot;
use uqa_core::memory::ProductionControl;
use uqa_sql::SQLError;

pub(crate) fn validate_callback_bindings(
    snapshot: &CatalogReadSnapshot,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    uqa_sql::catalog::foreign_wrapper::validate_functions_with_control(
        &snapshot.definitions.foreign_wrappers,
        &snapshot.definitions.sql_user_functions,
        control,
    )
}
