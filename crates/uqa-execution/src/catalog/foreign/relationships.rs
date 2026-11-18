//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate the selected foreign wrappers, SQL callbacks, and foreign tables in normal admission order.

use crate::catalog::{CatalogReadSnapshot, CatalogReadView};
use uqa_core::{
    image_io::{decode_image, AsyncImageInput, ImageDecoder},
    memory::{Budgeted, MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::SQLError;

pub(crate) type Input = Budgeted<Box<ImageDecoder<(), SQLError>>>;

pub(crate) fn has_bindings(snapshot: &CatalogReadSnapshot) -> bool {
    let definitions = &snapshot.definitions;
    !definitions.foreign_tables.is_empty()
        || !definitions.foreign_servers.is_empty()
        || (!definitions.foreign_wrappers.is_empty() && !definitions.sql_user_functions.is_empty())
}

pub(crate) fn begin(
    view: &CatalogReadView,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<Input, SQLError> {
    let header = memory.reserve(size_of::<ImageDecoder<(), SQLError>>())?;
    let view = view.clone();
    let original = cancellation.clone();
    let input = ImageDecoder::new(memory, cancellation, |mut reader| async move {
        validate(view.snapshot(), &original, &mut reader.input()).await
    })?;
    Ok(Budgeted::new(Box::new(input), header))
}

pub(crate) fn inline(
    snapshot: &CatalogReadSnapshot,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<(), SQLError> {
    decode_image(
        &mut std::io::empty(),
        memory,
        cancellation,
        async |mut input| validate(snapshot, cancellation, &mut input).await,
    )
}

async fn validate(
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
