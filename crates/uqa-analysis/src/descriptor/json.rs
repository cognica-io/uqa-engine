//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Duplicate-key admission precedes native wire construction in original property order.

use std::collections::BTreeSet;

use serde::de::{DeserializeOwned, Error as _};
use uqa_core::json::JsonToken;

use super::{DescriptorData, RuntimeProfiles, Wire};
use crate::{json::Input, work::Work, AnalysisError, AnalysisResult};

pub(super) async fn decode(
    json: &str,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<Wire> {
    unique_keys(json, work).await?;
    let mut input = Input::new(json.as_bytes());
    match wire(&mut input, work).await {
        Err(AnalysisError::Json(error)) if error.line() == 0 => {
            Err(input.located(error, work).await?)
        }
        result => result,
    }
}

async fn wire(input: &mut Input<'_>, work: &mut impl Work<AnalysisError>) -> AnalysisResult<Wire> {
    let mut fields = Fields::new(input, "Wire", &["descriptor", "fingerprint"], work).await?;
    let mut descriptor = None;
    let mut fingerprint = None;
    while let Some(field) = fields.next(input, work).await? {
        match field {
            0 => descriptor = Some(data(input, work).await?),
            1 => fingerprint = Some(typed(input, work).await?),
            _ => unreachable!(),
        }
    }
    if input.next(work).await?.is_some() {
        unreachable!("the original grammar admits one complete root");
    }
    Ok(Wire {
        descriptor: required(descriptor, "descriptor")?,
        fingerprint: required(fingerprint, "fingerprint")?,
    })
}

async fn unique_keys(json: &str, work: &mut impl Work<AnalysisError>) -> AnalysisResult<()> {
    let mut input = Input::new(json.as_bytes());
    let mut objects: Vec<BTreeSet<String>> = Vec::new();
    while let Some(token) = input.next(work).await? {
        match token {
            JsonToken::StartObject => objects.push(BTreeSet::new()),
            JsonToken::EndObject => {
                objects.pop().expect("the grammar owns object nesting");
            }
            JsonToken::Key(encoded) => {
                let key = input.string(encoded, work).await?;
                if !objects
                    .last_mut()
                    .expect("keys occur inside objects")
                    .insert(key)
                {
                    return Err(input
                        .located(
                            serde_json::Error::custom("duplicate analyzer descriptor property"),
                            work,
                        )
                        .await?);
                }
            }
            _ => (),
        }
    }
    Ok(())
}

async fn data(
    input: &mut Input<'_>,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<DescriptorData> {
    let mut fields = Fields::new(
        input,
        "DescriptorData",
        &[
            "format",
            "format_version",
            "algorithm_revision",
            "source_mapping_revision",
            "length_policy",
            "pipeline",
            "runtime_profiles",
        ],
        work,
    )
    .await?;
    let (mut format, mut format_version, mut algorithm_revision, mut source_mapping_revision) =
        (None, None, None, None);
    let (mut length_policy, mut pipeline, mut runtime_profiles) = (None, None, None);
    while let Some(field) = fields.next(input, work).await? {
        match field {
            0 => format = Some(typed(input, work).await?),
            1 => format_version = Some(typed(input, work).await?),
            2 => algorithm_revision = Some(typed(input, work).await?),
            3 => source_mapping_revision = Some(typed(input, work).await?),
            4 => length_policy = Some(typed(input, work).await?),
            5 => pipeline = Some(input.value(work).await?),
            6 => runtime_profiles = Some(profiles(input, work).await?),
            _ => unreachable!(),
        }
    }
    Ok(DescriptorData {
        format: required(format, "format")?,
        format_version: required(format_version, "format_version")?,
        algorithm_revision: required(algorithm_revision, "algorithm_revision")?,
        source_mapping_revision: required(source_mapping_revision, "source_mapping_revision")?,
        length_policy: required(length_policy, "length_policy")?,
        pipeline: required(pipeline, "pipeline")?,
        runtime_profiles: required(runtime_profiles, "runtime_profiles")?,
    })
}

async fn profiles(
    input: &mut Input<'_>,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<RuntimeProfiles> {
    let mut fields = Fields::new(
        input,
        "RuntimeProfiles",
        &["rust_unicode", "normalization_unicode", "expressions"],
        work,
    )
    .await?;
    let (mut rust_unicode, mut normalization_unicode, mut expressions) = (None, None, None);
    while let Some(field) = fields.next(input, work).await? {
        match field {
            0 => rust_unicode = Some(version(input, work).await?),
            1 => normalization_unicode = Some(version(input, work).await?),
            2 => {
                if input.peek(work).await? != Some(JsonToken::StartArray) {
                    expressions = Some(typed(input, work).await?);
                    continue;
                }
                input.next(work).await?;
                let mut values = Vec::new();
                while input.peek(work).await? != Some(JsonToken::EndArray) {
                    values.push(typed(input, work).await?);
                }
                input.next(work).await?;
                expressions = Some(values);
            }
            _ => unreachable!(),
        }
    }
    Ok(RuntimeProfiles {
        rust_unicode: required(rust_unicode, "rust_unicode")?,
        normalization_unicode: required(normalization_unicode, "normalization_unicode")?,
        expressions: required(expressions, "expressions")?,
    })
}

async fn version(
    input: &mut Input<'_>,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<Option<(u8, u8, u8)>> {
    if input.peek(work).await? == Some(JsonToken::Null) {
        input.next(work).await?;
        return Ok(None);
    }
    if input.peek(work).await? != Some(JsonToken::StartArray) {
        return typed(input, work).await;
    }
    input.next(work).await?;
    let mut version = [0; 3];
    for (index, component) in version.iter_mut().enumerate() {
        if input.peek(work).await? == Some(JsonToken::EndArray) {
            return Err(serde_json::Error::invalid_length(index, &"a tuple of size 3").into());
        }
        *component = typed(input, work).await?;
    }
    if input.next(work).await? != Some(JsonToken::EndArray) {
        return Err(serde_json::Error::custom("trailing characters").into());
    }
    Ok(Some((version[0], version[1], version[2])))
}

async fn typed<T: DeserializeOwned>(
    input: &mut Input<'_>,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<T> {
    let value = input.value(work).await?;
    match value {
        // Scalar conversion keeps the public JSON dependency's numeric diagnostics.
        serde_json::Value::Number(value) => serde_json::from_str(value.as_str()).map_err(|error| {
            serde_json::Error::custom(error.to_string().split(" at line ").next().unwrap()).into()
        }),
        value => Ok(serde_json::from_value(value)?),
    }
}

fn required<T>(value: Option<T>, field: &'static str) -> AnalysisResult<T> {
    value.ok_or_else(|| serde_json::Error::missing_field(field).into())
}

struct Fields {
    names: &'static [&'static str],
    name: &'static str,
    sequence: Option<usize>,
}

impl Fields {
    async fn new(
        input: &mut Input<'_>,
        name: &'static str,
        names: &'static [&'static str],
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<Self> {
        let sequence = match input.peek(work).await? {
            Some(JsonToken::StartObject) => None,
            Some(JsonToken::StartArray) => Some(0),
            _ => {
                let value = input.value(work).await?;
                let expected = format!("struct {name}");
                return Err(serde_json::Error::invalid_type(
                    unexpected(&value),
                    &expected.as_str(),
                )
                .into());
            }
        };
        input.next(work).await?;
        Ok(Self {
            names,
            name,
            sequence,
        })
    }

    async fn next(
        &mut self,
        input: &mut Input<'_>,
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<Option<usize>> {
        if let Some(index) = &mut self.sequence {
            if input.peek(work).await? == Some(JsonToken::EndArray) {
                input.next(work).await?;
                if *index != self.names.len() {
                    return Err(serde_json::Error::custom(format!(
                        "invalid length {index}, expected struct {} with {} elements",
                        self.name,
                        self.names.len()
                    ))
                    .into());
                }
                return Ok(None);
            }
            if *index == self.names.len() {
                return Err(serde_json::Error::custom("trailing characters").into());
            }
            let field = *index;
            *index += 1;
            return Ok(Some(field));
        }
        match input.next(work).await? {
            Some(JsonToken::EndObject) => Ok(None),
            Some(JsonToken::Key(encoded)) => {
                let key = input.string(encoded, work).await?;
                self.names
                    .iter()
                    .position(|name| *name == key)
                    .map(Some)
                    .ok_or_else(|| serde_json::Error::unknown_field(&key, self.names).into())
            }
            _ => unreachable!("the grammar requires an object key or closing delimiter"),
        }
    }
}

fn unexpected(value: &serde_json::Value) -> serde::de::Unexpected<'_> {
    match value {
        serde_json::Value::Null => serde::de::Unexpected::Unit,
        serde_json::Value::Bool(value) => serde::de::Unexpected::Bool(*value),
        serde_json::Value::String(value) => serde::de::Unexpected::Str(value),
        serde_json::Value::Number(value) => value
            .as_u64()
            .map(serde::de::Unexpected::Unsigned)
            .or_else(|| value.as_i64().map(serde::de::Unexpected::Signed))
            .or_else(|| value.as_f64().map(serde::de::Unexpected::Float))
            .unwrap_or(serde::de::Unexpected::Other("number")),
        serde_json::Value::Array(_) => serde::de::Unexpected::Seq,
        serde_json::Value::Object(_) => serde::de::Unexpected::Map,
    }
}

#[cfg(test)]
mod tests;
