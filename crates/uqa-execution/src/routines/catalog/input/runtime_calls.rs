//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read application call sites from the exact physical routine catalog record.

use super::{definitions::Slots, RoutineCatalogDocument};
use uqa_core::{image_io::AsyncImageInput, memory::ProductionControl};
use uqa_sql::{ast::CreateFunction, binding::view_dependencies::StoredFunctionSite, SQLError};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

/// Read the catalog framing and typed routine declarations through their existing constructors. The stored owner and permissions do not participate in application function selection.
pub async fn visit_stored_routine_runtime_calls<'a, 'input: 'a, F>(
    record: &'a [u8],
    source: &'a StorageReadControl,
    input: &'a mut AsyncImageInput<'input>,
    visit: &'a mut F,
) -> StorageBackendResult<()>
where
    F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send,
{
    let control =
        ProductionControl::new(source.memory(), source.cancellation(), input.cancellation);
    let document =
        RoutineCatalogDocument::read_record_with_work(record, false, &control, input).await?;
    let protocol = crate::catalog::type_records::marker::protocol::keys(&control)?;
    for group in Slots::new(document.definitions) {
        for value in Slots::new(group.overloads) {
            input.checkpoint().await?;
            let definition = CreateFunction::from_projected_json_with_work(
                value.root(),
                &control,
                input,
                protocol,
            )
            .await?;
            value.retire_with_work(&control, input).await?;
            uqa_sql::routines::native::visit_stored_routine_runtime_calls(
                &definition,
                None,
                source.cancellation(),
                input,
                visit,
            )
            .await
            .map_err(|error| match error {
                SQLError::Cancelled(reason) => reason.into(),
                error => StorageBackendError::CatalogBinding(std::sync::Arc::new(error)),
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
