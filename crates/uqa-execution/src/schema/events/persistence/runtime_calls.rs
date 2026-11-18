//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe the same executable event definitions on a committed physical catalog cut.

use crate::catalog::type_records::marker::protocol;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_with_work, JsonDocument, JsonInputError, JsonReader, JsonToken},
    memory::ProductionControl,
};
use uqa_sql::{
    ast::{CreateRule, CreateTrigger},
    binding::view_dependencies::StoredFunctionSite,
    catalog::events::persistence::rule_catalog_requires_migration,
    SQLError,
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

#[derive(Clone, Copy)]
enum Catalog {
    Rules,
    Triggers,
}

/// Read trigger WHEN expressions from the original catalog record under the source and invoking controls.
pub async fn visit_stored_trigger_runtime_calls<'a, 'input: 'a, F>(
    record: &'a [u8],
    source: &'a StorageReadControl,
    input: &'a mut AsyncImageInput<'input>,
    visit: &'a mut F,
) -> StorageBackendResult<()>
where
    F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send,
{
    Catalog::Triggers.visit(record, source, input, visit).await
}

/// Read the rule condition selected by the catalog and its action declarations under the same controls as ordinary stored SQL.
pub async fn visit_stored_rule_runtime_calls<'a, 'input: 'a, F>(
    record: &'a [u8],
    source: &'a StorageReadControl,
    input: &'a mut AsyncImageInput<'input>,
    visit: &'a mut F,
) -> StorageBackendResult<()>
where
    F: FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send,
{
    Catalog::Rules.visit(record, source, input, visit).await
}

impl Catalog {
    async fn entries<'a>(
        self,
        bytes: &'a [u8],
        control: &ProductionControl<'_>,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<&'a [u8]>> {
        Ok(match self {
            Self::Rules => {
                let [version, entries] =
                    fields(bytes, [("format_version", 0), ("rules", 1)], control, input).await?;
                if let Some(version) = version {
                    let version = JsonReader::from_slice_with_control(version, control)
                        .read_u32_from(input)
                        .await?;
                    rule_catalog_requires_migration(version, true)
                        .map_err(StorageBackendError::Other)?;
                }
                entries
            }
            Self::Triggers => {
                let [entries] = fields(bytes, [("triggers", 0)], control, input).await?;
                entries
            }
        })
    }

    async fn visit(
        self,
        bytes: &[u8],
        source: &StorageReadControl,
        input: &mut AsyncImageInput<'_>,
        visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
    ) -> StorageBackendResult<()> {
        let control =
            ProductionControl::new(source.memory(), source.cancellation(), input.cancellation);
        let entries = self.entries(bytes, &control, input).await?;
        let Some(entries) = entries else {
            return Ok(());
        };
        let keys = protocol::keys(&control)?;
        let mut reader = JsonReader::from_slice_with_control(entries, &control);
        if reader
            .next_event_with_work(input)
            .await
            .map_err(JsonInputError::from)?
            .map(|event| event.token)
            != Some(JsonToken::StartArray)
        {
            return Err(JsonInputError::Invalid.into());
        }
        while reader
            .next_array_value_with_work(input)
            .await
            .map_err(JsonInputError::from)?
        {
            let entry = reader
                .read_value_span_with_work(input)
                .await
                .map_err(JsonInputError::from)?;
            let (definition, plan) = match self {
                Self::Rules => {
                    let [definition, plan, binding] = fields(
                        entry,
                        [
                            ("definition", 0),
                            ("condition_plan", 2),
                            ("condition_binding", 3),
                        ],
                        &control,
                        input,
                    )
                    .await?;
                    (
                        definition,
                        plan.filter(|bytes| {
                            *bytes != b"null" && binding.is_some_and(|value| value != b"null")
                        }),
                    )
                }
                Self::Triggers => {
                    let [definition] = fields(entry, [("definition", 0)], &control, input).await?;
                    (definition, None)
                }
            };
            let document = JsonDocument::read_with_work(
                definition.ok_or(JsonInputError::Invalid)?,
                &control,
                input,
                keys,
            )
            .await?;
            let result = match self {
                Self::Triggers => {
                    let definition = CreateTrigger::from_projected_json_with_work(
                        document.root(),
                        &control,
                        input,
                        keys,
                    )
                    .await?;
                    document.retire_with_work(&control, input).await?;
                    definition
                        .visit_runtime_calls(source.cancellation(), input, visit)
                        .await
                }
                Self::Rules => {
                    let definition = CreateRule::from_projected_json_with_work(
                        document.root(),
                        &control,
                        input,
                        keys,
                    )
                    .await?;
                    document.retire_with_work(&control, input).await?;
                    definition
                        .visit_persisted_runtime_calls(plan, source.cancellation(), input, visit)
                        .await
                }
            };
            result.map_err(|error| match error {
                SQLError::Cancelled(reason) => reason.into(),
                error => StorageBackendError::CatalogBinding(std::sync::Arc::new(error)),
            })?;
        }
        Ok(())
    }
}

/// Each selected field borrows its original complete value. The catalog itself may hold arbitrarily many definitions; selecting its entries does not retain their trees or unknown keys.
async fn fields<'a, const N: usize>(
    bytes: &'a [u8],
    selection: [(&str, usize); N],
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> Result<[Option<&'a [u8]>; N], JsonInputError> {
    let mut reader = JsonReader::from_slice_with_control(bytes, control);
    let token = reader
        .next_event_with_work(input)
        .await?
        .ok_or(JsonInputError::Invalid)?
        .token;
    let mut fields = [None; N];
    match token {
        JsonToken::StartObject => loop {
            match reader
                .next_event_with_work(input)
                .await?
                .ok_or(JsonInputError::Invalid)?
                .token
            {
                JsonToken::EndObject => break,
                JsonToken::Key(key) => {
                    let key = decode_json_string_with_work(key, control, input).await?;
                    let field = selection.iter().position(|(name, _)| *name == key.as_str());
                    let value = reader.read_value_span_with_work(input).await?;
                    if let Some(index) = field {
                        if fields[index].replace(value).is_some() {
                            return Err(JsonInputError::Invalid);
                        }
                    }
                }
                _ => return Err(JsonInputError::Invalid),
            }
        },
        JsonToken::StartArray => {
            let mut position = 0;
            while reader.next_array_value_with_work(input).await? {
                let value = reader.read_value_span_with_work(input).await?;
                if let Some(index) = selection
                    .iter()
                    .position(|(_, ordinal)| *ordinal == position)
                {
                    fields[index] = Some(value);
                }
                position += 1;
            }
        }
        _ => return Err(JsonInputError::Invalid),
    }
    if reader.next_event_with_work(input).await?.is_some() {
        return Err(JsonInputError::Invalid);
    }
    Ok(fields)
}

#[cfg(test)]
mod tests;
