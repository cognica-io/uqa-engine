//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate the selected foreign wrappers, SQL callbacks, and foreign tables in normal admission order.

use crate::catalog::CatalogReadSnapshot;
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl, CancellationToken};
use uqa_sql::SQLError;

pub(crate) fn has_bindings(snapshot: &CatalogReadSnapshot) -> bool {
    let definitions = &snapshot.definitions;
    !definitions.foreign_tables.is_empty()
        || !definitions.foreign_servers.is_empty()
        || (!definitions.foreign_wrappers.is_empty() && !definitions.sql_user_functions.is_empty())
}

pub(crate) async fn validate(
    snapshot: &CatalogReadSnapshot,
    original: &CancellationToken,
    input: &mut AsyncImageInput<'_>,
) -> Result<(), SQLError> {
    let control = ProductionControl::new(input.memory, original, input.cancellation);
    let definitions = &snapshot.definitions;
    super::reference::wrappers_with_work(
        &definitions.foreign_servers,
        &definitions.foreign_wrappers,
        &control,
        input,
    )
    .await?;
    uqa_sql::catalog::foreign_wrapper::validate_functions_from_input(
        &definitions.foreign_wrappers,
        &definitions.sql_user_functions,
        &control,
        input,
    )
    .await?;
    super::reference::servers_with_work(
        definitions.foreign_tables.values(),
        &definitions.foreign_servers,
        &control,
        input,
    )
    .await
}
