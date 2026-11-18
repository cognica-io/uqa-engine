//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::catalog::type_records::marker::protocol;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{JsonDocument, JsonInputError, JsonValueKind},
    memory::ProductionControl,
    CancellationToken,
};
use uqa_sql::{
    ast::{ColumnDef, TableCheck},
    binding::view_dependencies::StoredFunctionSite,
    SQLError,
};
use uqa_storage::{
    backup::BackupTableField, read_control::StorageReadControl, StorageBackendResult,
};

/// Visit the scalar calls that the captured table can use for defaults, generated columns and checks.
pub async fn visit_table_functions(
    columns: &[ColumnDef],
    checks: &[TableCheck],
    original: &CancellationToken,
    work: &mut AsyncImageInput<'_>,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> Result<(), SQLError> {
    for column in columns {
        original.check()?;
        work.checkpoint().await?;
        for expression in column.stored_value_expressions() {
            expression
                .visit_runtime_scalar_calls(original, work, visit)
                .await?;
        }
    }
    for check in checks {
        original.check()?;
        work.checkpoint().await?;
        check
            .expr
            .visit_runtime_scalar_calls(original, work, visit)
            .await?;
    }
    Ok(())
}

/// Use the existing SQL declaration decoder for each stored element before visiting its calls.
pub async fn visit_stored_table_functions(
    bytes: &[u8],
    field: BackupTableField,
    source: &StorageReadControl,
    work: &mut AsyncImageInput<'_>,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> StorageBackendResult<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let control = ProductionControl::new(source.memory(), source.cancellation(), work.cancellation);
    let protocol = protocol::keys(&control)?;
    let document = JsonDocument::read_with_work(bytes, &control, work, protocol).await?;
    let root = document.root();
    let items = match field {
        BackupTableField::Columns => Some(root),
        BackupTableField::Constraints => match root.kind() {
            JsonValueKind::Object => root.get("checks"),
            JsonValueKind::Array => root.elements().and_then(|mut fields| fields.nth(2)),
            _ => return Err(JsonInputError::Invalid.into()),
        },
    };
    if let Some(items) = items {
        for item in items.elements().ok_or(JsonInputError::Invalid)? {
            work.checkpoint().await?;
            let observed = match field {
                BackupTableField::Columns => {
                    let column =
                        ColumnDef::from_projected_json_with_work(item, &control, work, protocol)
                            .await?;
                    visit_table_functions(
                        std::slice::from_ref(&*column),
                        &[],
                        source.cancellation(),
                        work,
                        visit,
                    )
                    .await
                }
                BackupTableField::Constraints => {
                    let check =
                        TableCheck::from_projected_json_with_work(item, &control, work, protocol)
                            .await?;
                    visit_table_functions(
                        &[],
                        std::slice::from_ref(&*check),
                        source.cancellation(),
                        work,
                        visit,
                    )
                    .await
                }
            };
            observed.map_err(|error| {
                uqa_storage::StorageBackendError::CatalogBinding(std::sync::Arc::new(error))
            })?;
        }
    }
    document.retire_with_work(&control, work).await?;
    Ok(())
}
