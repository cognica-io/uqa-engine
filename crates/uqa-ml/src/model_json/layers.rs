//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, Result};
use crate::{AggregationSpec, DeepLayerSpec, DirectionSpec, PoolKindSpec, PoolMethodSpec};
use uqa_core::{
    json::{
        JsonDiagnostic, JsonDocument, JsonInputError, JsonMessageFormat, JsonPosition, JsonReader,
        JsonValue,
    },
    memory::BudgetedVec,
};

impl Input<'_, '_> {
    pub(super) async fn layer(&mut self, reader: &mut JsonReader<'_, '_>) -> Result<DeepLayerSpec> {
        let document =
            JsonDocument::read_map_with_work(reader, &self.control, self.io, self.protocol).await?;
        let fields = document.root();
        let position = JsonPosition::unlocated();
        let kind = fields
            .get("kind")
            .ok_or_else(|| JsonDiagnostic::message("layer layer is missing `kind`", position))?
            .read_string_with_work(&self.control, self.io)
            .await
            .map_err(|error| {
                static KIND: JsonMessageFormat =
                    JsonMessageFormat::new("invalid `kind` in layer layer: ", "", "invalid kind");
                context(error, &KIND, position, self.io.memory)
            })?;
        macro_rules! field {
            ($kind:literal, $name:literal, $method:ident) => {{
                let value = fields.get($name).ok_or_else(|| {
                    JsonDiagnostic::message(
                        concat!($kind, " layer is missing `", $name, "`"),
                        position,
                    )
                })?;
                self.$method(value).await.map_err(|error| {
                    static FIELD: JsonMessageFormat = JsonMessageFormat::new(
                        concat!("invalid `", $name, "` in ", $kind, " layer: "),
                        "",
                        "invalid layer field",
                    );
                    context(error, &FIELD, position, self.io.memory)
                })
            }};
        }
        let layer = crate::model::fields::layer_fields!(kind.as_str(), field, {
            static UNKNOWN: JsonMessageFormat =
                JsonMessageFormat::new("unknown deep layer kind `", "`", "unknown deep layer kind");
            return Err(JsonDiagnostic::retained_message(&UNKNOWN, kind, position).into());
        });
        document.retire_with_work(&self.control, self.io).await?;
        Ok(layer)
    }

    async fn floats(&mut self, value: JsonValue<'_>) -> Result<Vec<f64>> {
        let Some(elements) = value.elements() else {
            return Err(JsonDiagnostic::projected_type_with_work(
                value,
                "a sequence",
                &self.control,
                self.io,
            )
            .await?
            .into());
        };
        let mut values = BudgetedVec::new(self.io.memory);
        for element in elements {
            values.push(element.read_f64_with_work(&self.control, self.io).await?)?;
        }
        let (values, memory) = values.into_parts();
        self.memory.absorb(memory);
        Ok(values)
    }

    async fn text(&mut self, value: JsonValue<'_>) -> Result<String> {
        let (text, memory) = value
            .read_string_with_work(&self.control, self.io)
            .await?
            .into_parts();
        self.memory.absorb(memory.expect("admitted model label"));
        Ok(text)
    }

    async fn count(&mut self, value: JsonValue<'_>) -> Result<usize> {
        value.read_usize_with_work(&self.control, self.io).await
    }

    async fn float(&mut self, value: JsonValue<'_>) -> Result<f64> {
        value.read_f64_with_work(&self.control, self.io).await
    }

    async fn boolean(&mut self, value: JsonValue<'_>) -> Result<bool> {
        value.read_bool_with_work(&self.control, self.io).await
    }

    async fn global_pool(&mut self, value: JsonValue<'_>) -> Result<PoolMethodSpec> {
        Ok(
            match value
                .read_unit_variant_with_work(&["avg", "max", "avg_max"], &self.control, self.io)
                .await?
            {
                0 => PoolMethodSpec::Avg,
                1 => PoolMethodSpec::Max,
                2 => PoolMethodSpec::AvgMax,
                _ => unreachable!("declared global pooling variant"),
            },
        )
    }

    async fn pool(&mut self, value: JsonValue<'_>) -> Result<PoolKindSpec> {
        Ok(
            match value
                .read_unit_variant_with_work(&["avg", "max"], &self.control, self.io)
                .await?
            {
                0 => PoolKindSpec::Avg,
                1 => PoolKindSpec::Max,
                _ => unreachable!("declared pooling variant"),
            },
        )
    }

    async fn aggregation(&mut self, value: JsonValue<'_>) -> Result<AggregationSpec> {
        Ok(
            match value
                .read_unit_variant_with_work(&["mean", "sum", "max"], &self.control, self.io)
                .await?
            {
                0 => AggregationSpec::Mean,
                1 => AggregationSpec::Sum,
                2 => AggregationSpec::Max,
                _ => unreachable!("declared aggregation variant"),
            },
        )
    }

    async fn direction(&mut self, value: JsonValue<'_>) -> Result<DirectionSpec> {
        Ok(
            match value
                .read_unit_variant_with_work(&["out", "in", "both"], &self.control, self.io)
                .await?
            {
                0 => DirectionSpec::Out,
                1 => DirectionSpec::In,
                2 => DirectionSpec::Both,
                _ => unreachable!("declared direction variant"),
            },
        )
    }
}

fn context(
    error: JsonInputError,
    format: &'static JsonMessageFormat,
    position: JsonPosition,
    memory: &uqa_core::memory::MemoryBudget,
) -> JsonInputError {
    match error {
        JsonInputError::Diagnostic(error) => match error.with_context(format, position, memory) {
            Ok(error) => error.into(),
            Err(error) => error.into(),
        },
        error => error,
    }
}
