//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical descriptor output borrows typed fields and the original pipeline value.

use super::{
    limits, AnalyzerFingerprint, AnalyzerLimits, DescriptorData, RuntimeProfiles, TokenLengthPolicy,
};
use crate::{
    json::{write_raw, write_string, write_value, Output},
    work::Work,
    AnalysisError, AnalysisResult,
};

struct Text {
    value: String,
    maximum: usize,
}

impl Text {
    fn new(limits: AnalyzerLimits) -> Self {
        Self {
            value: String::new(),
            maximum: limits.max_descriptor_bytes,
        }
    }
}

impl Output<AnalysisError> for Text {
    fn write_str(&mut self, value: &str) -> AnalysisResult<()> {
        let required = self
            .value
            .len()
            .checked_add(value.len())
            .ok_or_else(|| super::invalid("analyzer size overflow"))?;
        limits::check_limit("analyzer descriptor bytes", required, self.maximum)?;
        self.value.push_str(value);
        Ok(())
    }
}

pub(super) async fn data(
    data: &DescriptorData,
    limits: AnalyzerLimits,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<String> {
    let mut output = Text::new(limits);
    work.checkpoint().await?;
    output.write_str("{\"algorithm_revision\":")?;
    output.write_str(&data.algorithm_revision.to_string())?;
    output.write_str(",\"format\":")?;
    write_string(&data.format, &mut output, work).await?;
    output.write_str(",\"format_version\":")?;
    output.write_str(&data.format_version.to_string())?;
    output.write_str(",\"length_policy\":")?;
    output.write_str(match data.length_policy {
        TokenLengthPolicy::EmittedTokens => "\"emitted_tokens\"",
        TokenLengthPolicy::DiscountOverlaps => "\"discount_overlaps\"",
    })?;
    output.write_str(",\"pipeline\":")?;
    write_value(&data.pipeline, &mut output, work).await?;
    output.write_str(",\"runtime_profiles\":")?;
    profiles(&data.runtime_profiles, &mut output, work).await?;
    output.write_str(",\"source_mapping_revision\":")?;
    output.write_str(&data.source_mapping_revision.to_string())?;
    output.write_str("}")?;
    Ok(output.value)
}

async fn profiles(
    profiles: &RuntimeProfiles,
    output: &mut Text,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<()> {
    output.write_str("{\"expressions\":[")?;
    for (index, expression) in profiles.expressions.iter().enumerate() {
        work.records(index).await?;
        if index != 0 {
            output.write_str(",")?;
        }
        write_string(expression, output, work).await?;
    }
    output.write_str("],\"normalization_unicode\":")?;
    version(profiles.normalization_unicode, output)?;
    output.write_str(",\"rust_unicode\":")?;
    version(profiles.rust_unicode, output)?;
    output.write_str("}")
}

fn version(version: Option<(u8, u8, u8)>, output: &mut Text) -> AnalysisResult<()> {
    if let Some((major, minor, patch)) = version {
        output.write_str(&format!("[{major},{minor},{patch}]"))
    } else {
        output.write_str("null")
    }
}

pub(super) async fn wire(
    data: &str,
    fingerprint: AnalyzerFingerprint,
    limits: AnalyzerLimits,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<String> {
    let mut output = Text::new(limits);
    work.checkpoint().await?;
    output.write_str("{\"descriptor\":")?;
    write_raw(data, &mut output, work).await?;
    output.write_str(",\"fingerprint\":\"")?;
    output.write_str(&fingerprint.to_string())?;
    output.write_str("\"}")?;
    Ok(output.value)
}
