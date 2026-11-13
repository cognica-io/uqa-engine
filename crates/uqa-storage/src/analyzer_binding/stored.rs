//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored envelopes borrow exact descriptor text and admit only native binding fields.

use super::{AnalyzerBindingOwner, AnalyzerPhase, BoundAnalyzerRevision, FieldAnalyzerBinding};
use crate::{StorageBackendError, StorageBackendResult};
use serde::de::DeserializeOwned;
use uqa_analysis::AnalyzerResources;
use uqa_core::{
    image_io::AsyncImageInput,
    json::{decode_json_string_from, JsonEvent, JsonReadError, JsonReader, JsonToken},
    memory::Budgeted,
};

struct Input<'a, 'c> {
    text: &'a str,
    reader: JsonReader<'a, 'c>,
}

struct Fields {
    names: &'static [&'static str],
    sequence: Option<usize>,
    seen: u8,
}

struct Side<'a> {
    name: Option<Budgeted<String>>,
    descriptor: &'a str,
}

struct Binding<'a> {
    format: Budgeted<String>,
    version: u32,
    owner: AnalyzerBindingOwner,
    last_phase: AnalyzerPhase,
    index: Side<'a>,
    search: Side<'a>,
}

fn invalid() -> StorageBackendError {
    StorageBackendError::Serde(<serde_json::Error as serde::de::Error>::custom(
        "invalid stored field analyzer binding JSON",
    ))
}

fn json_error(error: JsonReadError) -> StorageBackendError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        _ => invalid(),
    }
}

impl<'a> Input<'a, '_> {
    async fn next(
        &mut self,
        work: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<JsonEvent<'a, &'a [u8]>> {
        self.reader
            .next_event_from(work)
            .await
            .map_err(json_error)?
            .ok_or_else(invalid)
    }

    async fn raw(&mut self, work: &mut AsyncImageInput<'_>) -> StorageBackendResult<&'a str> {
        let first = self.next(work).await?;
        let start = first.range.start;
        let mut end = first.range.end;
        let mut depth = usize::from(matches!(
            first.token,
            JsonToken::StartObject | JsonToken::StartArray
        ));
        while depth != 0 {
            let event = self.next(work).await?;
            end = event.range.end;
            match event.token {
                JsonToken::StartObject | JsonToken::StartArray => depth += 1,
                JsonToken::EndObject | JsonToken::EndArray => depth -= 1,
                _ => (),
            }
        }
        Ok(&self.text[start..end])
    }

    async fn atom<T: DeserializeOwned>(
        &mut self,
        work: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<T> {
        Ok(serde_json::from_str(self.raw(work).await?)?)
    }

    async fn text(
        &mut self,
        work: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Budgeted<String>> {
        let JsonToken::String(bytes) = self.next(work).await?.token else {
            return Err(invalid());
        };
        decode_json_string_from(bytes, work)
            .await
            .map_err(json_error)
    }

    async fn name(
        &mut self,
        work: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<Budgeted<String>>> {
        match self.next(work).await?.token {
            JsonToken::Null => Ok(None),
            JsonToken::String(bytes) => Ok(Some(
                decode_json_string_from(bytes, work)
                    .await
                    .map_err(json_error)?,
            )),
            _ => Err(invalid()),
        }
    }

    async fn fields(
        &mut self,
        names: &'static [&'static str],
        work: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Fields> {
        let sequence = match self.next(work).await?.token {
            JsonToken::StartObject => None,
            JsonToken::StartArray => Some(0),
            _ => return Err(invalid()),
        };
        Ok(Fields {
            names,
            sequence,
            seen: 0,
        })
    }

    async fn side(&mut self, work: &mut AsyncImageInput<'_>) -> StorageBackendResult<Side<'a>> {
        let mut fields = self.fields(&["name", "descriptor"], work).await?;
        let (mut name, mut descriptor) = (None, None);
        while let Some(field) = fields.next(self, work).await? {
            match field {
                0 => name = self.name(work).await?,
                1 => descriptor = Some(self.raw(work).await?),
                _ => unreachable!(),
            }
        }
        Ok(Side {
            name,
            descriptor: descriptor.ok_or_else(invalid)?,
        })
    }

    async fn binding(
        &mut self,
        work: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Binding<'a>> {
        let mut fields = self
            .fields(
                &[
                    "format",
                    "version",
                    "owner",
                    "last_phase",
                    "index",
                    "search",
                ],
                work,
            )
            .await?;
        let (mut format, mut version, mut owner, mut last_phase, mut index, mut search) =
            (None, None, None, None, None, None);
        while let Some(field) = fields.next(self, work).await? {
            match field {
                0 => format = Some(self.text(work).await?),
                1 => version = Some(self.atom(work).await?),
                2 => owner = Some(self.atom(work).await?),
                3 => last_phase = Some(self.atom(work).await?),
                4 => index = Some(self.side(work).await?),
                5 => search = Some(self.side(work).await?),
                _ => unreachable!(),
            }
        }
        if self
            .reader
            .next_event_from(work)
            .await
            .map_err(json_error)?
            .is_some()
        {
            return Err(invalid());
        }
        Ok(Binding {
            format: format.ok_or_else(invalid)?,
            version: version.ok_or_else(invalid)?,
            owner: owner.ok_or_else(invalid)?,
            last_phase: last_phase.ok_or_else(invalid)?,
            index: index.ok_or_else(invalid)?,
            search: search.ok_or_else(invalid)?,
        })
    }
}

impl Fields {
    async fn next(
        &mut self,
        input: &mut Input<'_, '_>,
        work: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Option<usize>> {
        if let Some(index) = &mut self.sequence {
            if *index == self.names.len() {
                if input.next(work).await?.token != JsonToken::EndArray {
                    return Err(invalid());
                }
                return Ok(None);
            }
            let field = *index;
            *index += 1;
            return Ok(Some(field));
        }
        let token = input.next(work).await?.token;
        if token == JsonToken::EndObject {
            return Ok(None);
        }
        let JsonToken::Key(bytes) = token else {
            return Err(invalid());
        };
        let name = decode_json_string_from(bytes, work)
            .await
            .map_err(json_error)?;
        let field = self
            .names
            .iter()
            .position(|field| *field == name.as_str())
            .ok_or_else(invalid)?;
        if self.seen & (1 << field) != 0 {
            return Err(invalid());
        }
        self.seen |= 1 << field;
        Ok(Some(field))
    }
}

impl FieldAnalyzerBinding {
    /// Decode the original closed stored envelope before restoring either exact revision. Descriptor spans borrow the original physical row; native names keep their admitted buffers through validation and publication. No intermediate image, generic JSON value tree or descriptor reserialization is constructed.
    pub async fn from_json_with_input(
        json: &str,
        resources: &AnalyzerResources,
        work: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<Budgeted<Self>> {
        if json.len() > resources.limits().max_descriptor_bytes.saturating_mul(3) {
            return Err(StorageBackendError::Other(
                "analyzer binding exceeds descriptor limits".into(),
            ));
        }
        let mut memory = work.memory.reserve(size_of::<Self>())?;
        let saved = Input {
            text: json,
            reader: JsonReader::new(json, work.memory, work.cancellation)
                .with_ignored_string_escapes(),
        }
        .binding(work)
        .await?;
        if saved.format.as_str() != "uqa-field-analyzer-binding" || saved.version != 1 {
            return Err(StorageBackendError::Other(
                "unsupported analyzer binding format".into(),
            ));
        }
        let index = resources
            .restore_json_with_input(saved.index.descriptor, work)
            .await?;
        let search = resources
            .restore_json_with_input(saved.search.descriptor, work)
            .await?;
        let mut name = |name: Option<Budgeted<String>>| {
            name.map(|name| {
                let (name, lease) = name.into_parts();
                memory.absorb(lease);
                name
            })
        };
        let binding = Self {
            index: BoundAnalyzerRevision {
                name: name(saved.index.name),
                compiled: index,
            },
            search: BoundAnalyzerRevision {
                name: name(saved.search.name),
                compiled: search,
            },
            owner: saved.owner,
            last_phase: saved.last_phase,
        };
        // Keep payloads inside their admitted owner across every remaining fallible operation.
        let binding = Budgeted::new(binding, memory);
        binding.validate()?;
        work.cancellation.check()?;
        Ok(binding)
    }
}

#[cfg(test)]
mod tests;
