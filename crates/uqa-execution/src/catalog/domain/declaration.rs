//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::catalog::type_records::marker::protocol;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{JsonDocument, JsonInputError, JsonObjectProjection, JsonReader, JsonToken},
    memory::{Produced, ProductionControl},
};
use uqa_sql::ast::CreateDomain;
use uqa_storage::{read_control::StorageReadControl, StorageBackendResult};

/// Read the SQL declaration of one stored domain from its original record. Named and positional records select the same field. Authority and physical identity stay with the domain catalog.
pub async fn read_stored_domain_declaration(
    record: &[u8],
    control: &StorageReadControl,
    work: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Produced<CreateDomain>> {
    let production =
        ProductionControl::new(control.memory(), control.cancellation(), work.cancellation);
    let mut reader = JsonReader::from_slice_with_control(record, &production);
    let kind = reader
        .next_event_with_work(work)
        .await
        .map_err(JsonInputError::from)?;
    let bytes = match kind.map(|event| event.token) {
        Some(JsonToken::StartObject) => {
            JsonObjectProjection::read_from(record, control.memory(), control.cancellation(), work)
                .await
                .map_err(JsonInputError::from)?
                .get("definition")
                .ok_or(JsonInputError::Invalid)?
        }
        Some(JsonToken::StartArray) => {
            // In the positional encoding five identity and ownership fields precede the declaration.
            for _ in 0..5 {
                reader
                    .read_value_span_with_work(work)
                    .await
                    .map_err(JsonInputError::from)?;
            }
            reader
                .read_value_span_with_work(work)
                .await
                .map_err(JsonInputError::from)?
        }
        _ => return Err(JsonInputError::Invalid.into()),
    };
    let protocol = protocol::keys(&production)?;
    let document = JsonDocument::read_with_work(bytes, &production, work, protocol).await?;
    let declaration =
        CreateDomain::from_projected_json_with_work(document.root(), &production, work, protocol)
            .await?;
    document.retire_with_work(&production, work).await?;
    Ok(declaration)
}
