//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Duplicate-key admission precedes native wire construction in original property order.

use std::collections::BTreeSet;

use uqa_core::json::{JsonDiagnostic, JsonToken};

use super::{DescriptorData, RuntimeProfiles, TokenLengthPolicy, Wire};
use crate::{json::Input, work::Work, AnalysisError, AnalysisResult};

pub(super) async fn decode(
    json: &str,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<Wire> {
    unique_keys(json, work).await?;
    let mut input = Input::new(json.as_bytes());
    let value = wire(&mut input, work).await?;
    if input.next(work).await?.is_some() {
        unreachable!("the original grammar admits one complete root");
    }
    Ok(value)
}

async fn wire(input: &mut Input<'_>, work: &mut impl Work<AnalysisError>) -> AnalysisResult<Wire> {
    let mut fields = Fields::new(
        input,
        "struct Wire",
        "struct Wire with 2 elements",
        &["descriptor", "fingerprint"],
        work,
    )
    .await?;
    let mut descriptor = None;
    let mut fingerprint = None;
    while let Some(field) = fields.next(input, work).await? {
        match field {
            0 => descriptor = Some(data(input, work).await?),
            1 => {
                let value = input.text(work).await?;
                fingerprint = Some(match value.parse() {
                    Ok(value) => value,
                    Err(AnalysisError::Descriptor(reason)) => {
                        input.finish_struct_error(work).await?;
                        return Err(JsonDiagnostic::prefixed_message(
                            "invalid analyzer descriptor: ",
                            reason,
                            input.position(),
                        )
                        .into());
                    }
                    Err(error) => return Err(error),
                });
            }
            _ => unreachable!(),
        }
    }
    Ok(Wire {
        descriptor: required(descriptor, "descriptor", input)?,
        fingerprint: required(fingerprint, "fingerprint", input)?,
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
                    input.finish_struct_error(work).await?;
                    return Err(JsonDiagnostic::message(
                        "duplicate analyzer descriptor property",
                        input.position(),
                    )
                    .into());
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
        "struct DescriptorData",
        "struct DescriptorData with 7 elements",
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
            0 => format = Some(input.text(work).await?),
            1 => format_version = Some(input.u32(work).await?),
            2 => algorithm_revision = Some(input.u32(work).await?),
            3 => source_mapping_revision = Some(input.u32(work).await?),
            4 => {
                length_policy = Some(
                    match input
                        .unit_variant(&["emitted_tokens", "discount_overlaps"], work)
                        .await?
                    {
                        0 => TokenLengthPolicy::EmittedTokens,
                        1 => TokenLengthPolicy::DiscountOverlaps,
                        _ => unreachable!(),
                    },
                );
            }
            5 => pipeline = Some(input.value(work).await?),
            6 => runtime_profiles = Some(profiles(input, work).await?),
            _ => unreachable!(),
        }
    }
    Ok(DescriptorData {
        format: required(format, "format", input)?,
        format_version: required(format_version, "format_version", input)?,
        algorithm_revision: required(algorithm_revision, "algorithm_revision", input)?,
        source_mapping_revision: required(
            source_mapping_revision,
            "source_mapping_revision",
            input,
        )?,
        length_policy: required(length_policy, "length_policy", input)?,
        pipeline: required(pipeline, "pipeline", input)?,
        runtime_profiles: required(runtime_profiles, "runtime_profiles", input)?,
    })
}

async fn profiles(
    input: &mut Input<'_>,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<RuntimeProfiles> {
    let mut fields = Fields::new(
        input,
        "struct RuntimeProfiles",
        "struct RuntimeProfiles with 3 elements",
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
                let token = input.next(work).await?.expect("required expressions");
                if token != JsonToken::StartArray {
                    return Err(input.invalid_type(token, "a sequence", work).await?);
                }
                let mut values = Vec::new();
                while input.next_array_value(work).await? {
                    values.push(input.text(work).await?);
                }
                expressions = Some(values);
            }
            _ => unreachable!(),
        }
    }
    Ok(RuntimeProfiles {
        rust_unicode: required(rust_unicode, "rust_unicode", input)?,
        normalization_unicode: required(normalization_unicode, "normalization_unicode", input)?,
        expressions: required(expressions, "expressions", input)?,
    })
}

async fn version(
    input: &mut Input<'_>,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<Option<(u8, u8, u8)>> {
    let token = input.next(work).await?.expect("required version");
    if token == JsonToken::Null {
        return Ok(None);
    }
    if token != JsonToken::StartArray {
        return Err(input.invalid_type(token, "a tuple of size 3", work).await?);
    }
    let mut version = [0; 3];
    for (index, component) in version.iter_mut().enumerate() {
        if !input.next_array_value(work).await? {
            return Err(JsonDiagnostic::invalid_length(
                index,
                "a tuple of size 3",
                input.position(),
            )
            .into());
        }
        *component = input.u8(work).await?;
    }
    input.finish_array(work).await?;
    Ok(Some((version[0], version[1], version[2])))
}

fn required<T>(value: Option<T>, field: &'static str, input: &Input<'_>) -> AnalysisResult<T> {
    value.ok_or_else(|| JsonDiagnostic::missing_field(field, input.position()).into())
}

struct Fields {
    names: &'static [&'static str],
    expected_length: &'static str,
    sequence: Option<usize>,
}

impl Fields {
    async fn new(
        input: &mut Input<'_>,
        expected: &'static str,
        expected_length: &'static str,
        names: &'static [&'static str],
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<Self> {
        let sequence = match input.next(work).await?.expect("required struct") {
            JsonToken::StartObject => None,
            JsonToken::StartArray => Some(0),
            token => return Err(input.invalid_type(token, expected, work).await?),
        };
        Ok(Self {
            names,
            expected_length,
            sequence,
        })
    }

    async fn next(
        &mut self,
        input: &mut Input<'_>,
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<Option<usize>> {
        if let Some(index) = &mut self.sequence {
            if *index == self.names.len() {
                input.finish_array(work).await?;
                return Ok(None);
            }
            if !input.next_array_value(work).await? {
                return Err(JsonDiagnostic::invalid_length(
                    *index,
                    self.expected_length,
                    input.position(),
                )
                .into());
            }
            let field = *index;
            *index += 1;
            return Ok(Some(field));
        }
        match input.next(work).await? {
            Some(JsonToken::EndObject) => Ok(None),
            Some(JsonToken::Key(encoded)) => {
                let key = input.name(encoded, work).await?;
                self.names
                    .iter()
                    .position(|name| *name == key.as_str())
                    .map(Some)
                    .ok_or_else(|| {
                        JsonDiagnostic::unknown_field(key, self.names, input.position()).into()
                    })
            }
            _ => unreachable!("the grammar requires an object key or closing delimiter"),
        }
    }
}

#[cfg(test)]
mod tests;
