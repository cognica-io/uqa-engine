//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored model JSON retains the original typed and generic conversion boundaries.

use crate::{DeepLayerSpec, DeepModel, GatingSpec};
use uqa_core::{
    image_io::AsyncImageInput,
    json::{
        decode_json_string_from, JsonDiagnostic, JsonInputError, JsonReader, JsonScalarProtocol,
        JsonToken,
    },
    memory::{Budgeted, BudgetedVec, MemoryReservation, ProductionControl},
};

mod layers;

type Result<T> = std::result::Result<T, JsonInputError>;
type Token<'a> = JsonToken<'a, &'a [u8]>;

pub(crate) async fn read_model_json_from(
    text: &str,
    io: &mut AsyncImageInput<'_>,
) -> Result<Budgeted<DeepModel>> {
    io.checkpoint().await?;
    let control = ProductionControl::new(io.memory, io.cancellation, io.cancellation);
    let memory = io.memory.reserve(size_of::<DeepModel>())?;
    let scalar_memory = io.memory.reserve(256)?;
    let protocol = JsonScalarProtocol::from_values(
        &serde_json::Number::from(1_u8),
        serde_json::value::RawValue::NULL,
    )
    .expect("enabled public generic scalar representations");
    drop(scalar_memory);
    let mut reader =
        JsonReader::from_slice_with_control(text.as_bytes(), &control).with_depth_limit(127);
    let mut input = Input {
        io,
        control,
        memory,
        protocol,
    };
    let model = match input.next(&mut reader).await? {
        JsonToken::StartObject => input.object(&mut reader).await?,
        JsonToken::StartArray => input.sequence(&mut reader).await?,
        token => {
            return Err(input
                .invalid_type(&reader, token, "struct DeepModel")
                .await?)
        }
    };
    if reader
        .next_event_from(input.io)
        .await
        .map_err(|error| JsonInputError::from_reader(&reader, error))?
        .is_some()
    {
        return Err(JsonInputError::Invalid);
    }
    Ok(Budgeted::new(model, input.memory))
}

struct Input<'a, 'c> {
    io: &'a mut AsyncImageInput<'c>,
    control: ProductionControl<'c>,
    memory: MemoryReservation,
    protocol: JsonScalarProtocol,
}

impl Input<'_, '_> {
    async fn next<'a>(&mut self, reader: &mut JsonReader<'a, '_>) -> Result<Token<'a>> {
        reader
            .next_event_from(self.io)
            .await
            .map_err(|error| JsonInputError::from_reader(reader, error))?
            .map(|event| event.token)
            .ok_or(JsonInputError::Invalid)
    }

    async fn invalid_type(
        &mut self,
        reader: &JsonReader<'_, '_>,
        token: Token<'_>,
        expected: &'static str,
    ) -> Result<JsonInputError> {
        Ok(
            JsonDiagnostic::invalid_type_from(token, reader.position(), expected, self.io)
                .await?
                .into(),
        )
    }

    async fn object(&mut self, reader: &mut JsonReader<'_, '_>) -> Result<DeepModel> {
        let (mut layers, mut alpha, mut gating) = (None, None, None);
        loop {
            let token = self.next(reader).await?;
            let JsonToken::Key(encoded) = token else {
                if matches!(token, JsonToken::EndObject) {
                    break;
                }
                return Err(JsonInputError::Invalid);
            };
            let key = decode_json_string_from(encoded, self.io).await?;
            let duplicate = match key.as_str() {
                "layers" if layers.is_some() => Some("layers"),
                "alpha" if alpha.is_some() => Some("alpha"),
                "gating" if gating.is_some() => Some("gating"),
                _ => None,
            };
            if let Some(name) = duplicate {
                return Err(JsonDiagnostic::duplicate_field(name, reader.position()).into());
            }
            match key.as_str() {
                "layers" => layers = Some(self.layers(reader).await?),
                "alpha" => alpha = Some(reader.read_f64_from(self.io).await?),
                "gating" => gating = Some(self.gating(reader).await?),
                _ => {
                    reader
                        .read_raw_value_with_work(self.io)
                        .await
                        .map_err(|error| JsonInputError::from_reader(reader, error))?;
                }
            }
        }
        Ok(DeepModel {
            layers: layers
                .ok_or_else(|| JsonDiagnostic::missing_field("layers", reader.position()))?,
            alpha: alpha.unwrap_or_default(),
            gating: gating.unwrap_or_default(),
        })
    }

    async fn element(&mut self, reader: &mut JsonReader<'_, '_>) -> Result<bool> {
        reader
            .next_array_value_with_work(self.io)
            .await
            .map_err(|error| JsonInputError::from_reader(reader, error))
    }

    async fn sequence(&mut self, reader: &mut JsonReader<'_, '_>) -> Result<DeepModel> {
        if !self.element(reader).await? {
            return Err(JsonDiagnostic::invalid_field_length(
                0,
                "struct DeepModel",
                3,
                reader.position(),
            )
            .into());
        }
        let layers = self.layers(reader).await?;
        let alpha = if self.element(reader).await? {
            reader.read_f64_from(self.io).await?
        } else {
            return Ok(DeepModel {
                layers,
                alpha: 0.0,
                gating: GatingSpec::None,
            });
        };
        let gating = if self.element(reader).await? {
            self.gating(reader).await?
        } else {
            return Ok(DeepModel {
                layers,
                alpha,
                gating: GatingSpec::None,
            });
        };
        reader
            .finish_array_with_work(self.io)
            .await
            .map_err(|error| JsonInputError::from_reader(reader, error))?;
        Ok(DeepModel {
            layers,
            alpha,
            gating,
        })
    }

    async fn layers(&mut self, reader: &mut JsonReader<'_, '_>) -> Result<Vec<DeepLayerSpec>> {
        let token = self.next(reader).await?;
        if !matches!(token, JsonToken::StartArray) {
            return Err(self.invalid_type(reader, token, "a sequence").await?);
        }
        let mut layers = BudgetedVec::new(self.io.memory);
        while self.element(reader).await? {
            match self.layer(reader).await {
                Ok(layer) => layers.push(layer)?,
                Err(JsonInputError::Diagnostic(error)) if error.position().line == 0 => {
                    let position = reader.finish_struct_error_with_work(self.io).await?;
                    return Err(error.at_position(position).into());
                }
                Err(error) => return Err(error),
            }
        }
        let (layers, memory) = layers.into_parts();
        self.memory.absorb(memory);
        Ok(layers)
    }

    async fn gating(&mut self, reader: &mut JsonReader<'_, '_>) -> Result<GatingSpec> {
        Ok(
            match reader
                .read_unit_variant_from(&["none", "re_l_u", "swish"], self.io)
                .await?
            {
                0 => GatingSpec::None,
                1 => GatingSpec::ReLU,
                2 => GatingSpec::Swish,
                _ => unreachable!("declared gating variant"),
            },
        )
    }
}

#[cfg(test)]
mod tests;
