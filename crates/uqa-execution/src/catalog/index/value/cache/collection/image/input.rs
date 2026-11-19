//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The collection owns name admission and registration through the last declared value.

use super::{invalid, MemoryValueIndexesBuilder};
use crate::catalog::index::value::cache::{image::ValueIndexImageBuilder, storage};
use uqa_core::{
    image_io::{AsyncImageInput, ImageDecoder},
    memory::Budgeted,
    DocId,
};
use uqa_sql::SQLError;

pub(super) struct Selected {
    name: Budgeted<String>,
    index: Budgeted<Box<ValueIndexImageBuilder>>,
}

impl MemoryValueIndexesBuilder {
    /// Admit a unique index name and its native metadata through one retained channel. Empty indexes finish source registration before the metadata component completes.
    pub fn image_input(
        self,
        name: impl AsRef<str> + Send + Sync + 'static,
        limit: usize,
    ) -> Result<ImageDecoder<Self, SQLError>, SQLError> {
        self.control.check().map_err(storage)?;
        if self.selected.is_some() {
            return Err(storage(invalid(
                "evaluated index is missing declared values",
            )));
        }
        let control = self.control.clone();
        Ok(ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut input = reader.input();
                let name = input.copy_text(name.as_ref()).await?;
                let memory = input.memory.reserve(size_of::<ValueIndexImageBuilder>())?;
                let index =
                    ValueIndexImageBuilder::read_from(limit, &self.control, &mut input).await?;
                self.accept_input(name, Budgeted::new(Box::new(index), memory), &mut input)
                    .await
            },
        )?)
    }

    /// Admit the next value of the selected name. The last declared value retains its source until native registration finishes; later values cannot reopen the completed index.
    pub fn value_input(
        mut self,
        name: &str,
        id: DocId,
    ) -> Result<ImageDecoder<Self, SQLError>, SQLError> {
        self.control.check().map_err(storage)?;
        let selected = self.selected.take().ok_or_else(|| {
            storage(invalid(
                "evaluated value has no incomplete index definition",
            ))
        })?;
        if selected.name.as_str() != name {
            return Err(storage(invalid("evaluated value belongs to another index")));
        }
        let control = self.control.clone();
        Ok(ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut input = reader.input();
                let (index, memory) = selected.index.into_parts();
                let index = index.read_value_from(id, &mut input).await?;
                self.accept_input(
                    selected.name,
                    Budgeted::new(Box::new(index), memory),
                    &mut input,
                )
                .await
            },
        )?)
    }

    async fn accept_input(
        mut self,
        name: Budgeted<String>,
        index: Budgeted<Box<ValueIndexImageBuilder>>,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        if index.is_complete() {
            let (index, _memory) = index.into_parts();
            self.add_index_from(name, index.finish()?, input)
                .await
                .map_err(storage)
        } else {
            self.selected = Some(Selected { name, index });
            Ok(self)
        }
    }
}
