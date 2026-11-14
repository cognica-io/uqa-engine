//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete physical and generic routine catalog input precedes typed authority and syntax.

use std::sync::Arc;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};
use uqa_core::{
    image_io::AsyncImageInput,
    json::{
        JsonDiagnostic, JsonDocument, JsonInputError, JsonPosition, JsonReadError, JsonReadWork,
        JsonScalarProtocol, JsonValue,
    },
    memory::{Budgeted, BudgetedVec, MemoryBudget, ProductionControl, ProductionString},
    CancellationToken,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

mod authority;
pub use authority::RoutineCatalogAuthority;
mod definitions;
pub use definitions::RoutineCatalogDefinitions;
mod canonical;
pub use canonical::{
    RoutineCatalogCanonicalInput, RoutineCatalogTypeNames, RoutineRestoreSchemaInput,
};

/// Select and retain the physical routine record before interpreting any definition. A failed or abandoned input is terminal and publishes no registry state.
pub struct RoutineCatalogInput {
    input: ImageDecoder<RoutineCatalogDocument, StorageBackendError>,
}

impl RoutineCatalogInput {
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        limits: CatalogInputLimits,
        allows_migration: bool,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                read_physical(
                    catalog.as_ref(),
                    limits,
                    allows_migration,
                    retained.memory(),
                    &retained,
                    &mut work,
                )
                .await
            },
        )?;
        Ok(Self { input })
    }

    pub fn advance(
        &mut self,
        steps: usize,
    ) -> StorageBackendResult<Option<RoutineCatalogDocument>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

async fn read_physical(
    catalog: &dyn CatalogFacade,
    limits: CatalogInputLimits,
    allows_migration: bool,
    memory: &MemoryBudget,
    control: &StorageReadControl,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<RoutineCatalogDocument> {
    input.checkpoint().await?;
    let json = catalog.get_metadata_with_control(super::FUNCTIONS_METADATA_KEY, limits, control)?;
    RoutineCatalogDocument::read_from(
        json.as_deref().map(String::as_str),
        allows_migration,
        memory,
        control.cancellation(),
        input,
    )
    .await
}

struct DefinitionGroup {
    name: Budgeted<String>,
    overloads: BudgetedVec<JsonDocument>,
}

/// Admitted generic declarations after complete wrapper conversion. Authority binding, typed syntax and registry publication follow this boundary.
pub struct RoutineCatalogDocument {
    format: u32,
    definitions: BudgetedVec<DefinitionGroup>,
}

impl RoutineCatalogDocument {
    pub fn format(&self) -> u32 {
        self.format
    }

    pub fn definitions(&self) -> impl ExactSizeIterator<Item = (&str, &[JsonDocument])> {
        self.definitions
            .iter()
            .map(|group| (group.name.as_str(), &*group.overloads))
    }

    /// Preserve the original two generic-value boundaries and version/migration ordering through the current operation's continuation.
    pub async fn read_from(
        json: Option<&str>,
        allows_migration: bool,
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Self> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        Self::read_with_work(json, allows_migration, &control, input).await
    }

    pub async fn read_with_work<W: JsonReadWork<Error = JsonReadError>>(
        json: Option<&str>,
        allows_migration: bool,
        control: &ProductionControl<'_>,
        work: &mut W,
    ) -> StorageBackendResult<Self> {
        let protocol = crate::catalog::type_records::marker::protocol::keys(control)?;
        let document =
            JsonDocument::read_with_work(json.unwrap_or("{}").as_bytes(), control, work, protocol)
                .await?;
        let root = document.root();
        let (format, definitions) = if root.get("routine_catalog_format").is_some() {
            let mut format = None;
            let mut definitions = None;
            for (name, value) in root.fields().expect("selected wrapper is an object") {
                checkpoint(control, work).await?;
                match name {
                    "routine_catalog_format" => {
                        format = Some(value.read_u32_with_work(control, work).await?);
                    }
                    "definitions" => {
                        definitions = Some(read_definitions(value, control, work, protocol).await?);
                    }
                    _ => {
                        let name = copy_text(name, control, work).await?;
                        return Err(JsonDiagnostic::unknown_field(
                            name,
                            &["routine_catalog_format", "definitions"],
                            JsonPosition::unlocated(),
                        )
                        .into());
                    }
                }
            }
            let format = format.expect("selected version field");
            let definitions = definitions.ok_or_else(|| {
                JsonDiagnostic::missing_field("definitions", JsonPosition::unlocated())
            })?;
            super::encoding::validate_format(format)?;
            (format, definitions)
        } else {
            (0, read_definitions(root, control, work, protocol).await?)
        };
        super::encoding::validate_migration(format, allows_migration)?;
        control.check()?;
        Ok(Self {
            format,
            definitions,
        })
    }
}

async fn read_definitions<W: JsonReadWork<Error = JsonReadError>>(
    value: JsonValue<'_>,
    control: &ProductionControl<'_>,
    work: &mut W,
    protocol: JsonScalarProtocol,
) -> StorageBackendResult<BudgetedVec<DefinitionGroup>> {
    let Some(fields) = value.fields() else {
        return Err(
            JsonDiagnostic::projected_type_with_work(value, "a map", control, work)
                .await?
                .into(),
        );
    };
    let memory = control.budget().expect("admitted routine catalog input");
    let mut definitions = BudgetedVec::new(memory);
    for (name, values) in fields {
        checkpoint(control, work).await?;
        let name = copy_text(name, control, work).await?;
        let Some(items) = values.elements() else {
            return Err(JsonDiagnostic::projected_type_with_work(
                values,
                "a sequence",
                control,
                work,
            )
            .await?
            .into());
        };
        let mut overloads = BudgetedVec::new(memory);
        for value in items {
            checkpoint(control, work).await?;
            overloads.push(value.project_with_work(control, work, protocol).await?)?;
        }
        definitions.push(DefinitionGroup { name, overloads })?;
    }
    Ok(definitions)
}

async fn copy_text<W: JsonReadWork<Error = JsonReadError>>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> StorageBackendResult<Budgeted<String>> {
    let mut output = ProductionString::new(*control);
    let mut offset = 0;
    while offset < text.len() {
        checkpoint(control, work).await?;
        let mut end = (offset + 4096).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        output.push_str(&text[offset..end])?;
        offset = end;
    }
    Ok(output
        .finish()?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("admitted routine name")))
}

async fn checkpoint<W: JsonReadWork<Error = JsonReadError>>(
    control: &ProductionControl<'_>,
    work: &mut W,
) -> StorageBackendResult<()> {
    control.check()?;
    work.checkpoint()
        .await
        .map_err(JsonInputError::from)
        .map_err(StorageBackendError::from)
}

#[cfg(test)]
mod tests;
