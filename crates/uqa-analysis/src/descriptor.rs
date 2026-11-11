//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned resolved analyzer inputs, runtime profiles, and canonical JSON identity.

use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{value::RawValue, Value};

use crate::{AnalysisError, AnalysisResult, Analyzer};

mod config;
#[cfg(test)]
pub(crate) use config::check_config as check_configuration;
#[cfg(feature = "kuromoji")]
pub(crate) use config::check_config_with as check_configuration_with;
mod hash;
mod json;
pub(crate) mod limits;
mod output;
mod profiles;

pub use hash::AnalyzerFingerprint;
pub use limits::AnalyzerLimits;
use profiles::RuntimeProfiles;

const FORMAT: &str = "uqa-analyzer";
const FORMAT_VERSION: u32 = 1;
const ALGORITHM_REVISION: u32 = 1;
const SOURCE_MAPPING_REVISION: u32 = 1;

/// The declared field-length policy contributes to analyzer revision identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenLengthPolicy {
    /// Count every emitted token, including stacked alternatives.
    EmittedTokens,
    /// Count only emitted tokens with a positive position increment.
    DiscountOverlaps,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DescriptorData {
    format: String,
    format_version: u32,
    algorithm_revision: u32,
    source_mapping_revision: u32,
    length_policy: TokenLengthPolicy,
    pipeline: Value,
    runtime_profiles: RuntimeProfiles,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    descriptor: DescriptorData,
    fingerprint: AnalyzerFingerprint,
}

/// Immutable resolved JSON with a verified fingerprint and explicit compatibility revisions.
#[derive(Debug)]
pub struct AnalyzerDescriptor {
    data: DescriptorData,
    fingerprint: AnalyzerFingerprint,
    wire: Box<RawValue>,
}

pub(crate) struct ResolvedDescriptor {
    pub descriptor: Arc<AnalyzerDescriptor>,
    #[cfg(feature = "nori")]
    pub nori: crate::nori::pipeline::ResolvedNoriPipeline,
    #[cfg(feature = "kuromoji")]
    pub kuromoji: crate::kuromoji::pipeline::ResolvedKuromojiPipeline,
}

impl Serialize for AnalyzerDescriptor {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.wire.serialize(serializer)
    }
}

impl AnalyzerDescriptor {
    pub fn resolve(
        config: &Analyzer,
        length_policy: TokenLengthPolicy,
        limits: AnalyzerLimits,
    ) -> AnalysisResult<Arc<Self>> {
        Ok(Self::resolve_inputs(
            config,
            length_policy,
            limits,
            #[cfg(feature = "nori")]
            &crate::nori::NoriResources::default(),
            #[cfg(feature = "kuromoji")]
            &crate::kuromoji::KuromojiResources::default(),
        )?
        .descriptor)
    }

    pub(crate) fn resolve_inputs(
        config: &Analyzer,
        length_policy: TokenLengthPolicy,
        limits: AnalyzerLimits,
        #[cfg(feature = "nori")] resources: &crate::nori::NoriResources,
        #[cfg(feature = "kuromoji")] kuromoji_resources: &crate::kuromoji::KuromojiResources,
    ) -> AnalysisResult<ResolvedDescriptor> {
        config::check_config(config, limits)?;
        let profiles = RuntimeProfiles::resolve(config)?;
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        let mut resolved_config = config.clone();
        #[cfg(feature = "nori")]
        let nori =
            crate::nori::pipeline::ResolvedNoriPipeline::resolve(&mut resolved_config, resources)?;
        #[cfg(feature = "kuromoji")]
        let kuromoji = crate::kuromoji::pipeline::ResolvedKuromojiPipeline::resolve(
            &mut resolved_config,
            kuromoji_resources,
            limits,
        )?;
        #[cfg(any(feature = "nori", feature = "kuromoji"))]
        let config = &resolved_config;
        let pipeline = config::snapshot(config, limits)?;
        let descriptor = Self::finish(
            DescriptorData {
                format: FORMAT.into(),
                format_version: FORMAT_VERSION,
                algorithm_revision: ALGORITHM_REVISION,
                source_mapping_revision: SOURCE_MAPPING_REVISION,
                length_policy,
                pipeline,
                runtime_profiles: profiles,
            },
            limits,
        )?;
        Ok(ResolvedDescriptor {
            descriptor,
            #[cfg(feature = "nori")]
            nori,
            #[cfg(feature = "kuromoji")]
            kuromoji,
        })
    }

    /// Restore resolved inputs without opening any file or substituting current mutable definitions.
    pub fn from_json(json: &str, limits: AnalyzerLimits) -> AnalysisResult<Arc<Self>> {
        crate::work::inline(Self::from_json_with(json, limits, &mut ()))
    }

    /// Validate exact descriptor syntax, fields, fingerprints and runtime profiles through retained input work. The JSON is already admitted by its enclosing owner; this operation never reads the transport.
    pub async fn from_json_with_input(
        json: &str,
        limits: AnalyzerLimits,
        input: &mut uqa_core::image_io::AsyncImageInput<'_>,
    ) -> AnalysisResult<Arc<Self>> {
        Self::from_json_with(json, limits, input).await
    }

    pub(crate) async fn from_json_with(
        json: &str,
        limits: AnalyzerLimits,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<Arc<Self>> {
        limits::check_limit(
            "analyzer descriptor bytes",
            json.len(),
            limits.max_descriptor_bytes,
        )?;
        let wire = json::decode(json, work).await?;
        let data_bytes = output::data(&wire.descriptor, limits, work).await?;
        let fingerprint = AnalyzerFingerprint::digest_with(data_bytes.as_bytes(), work).await?;
        if wire.fingerprint != fingerprint {
            return Err(AnalysisError::DescriptorFingerprint {
                expected: wire.fingerprint,
                actual: fingerprint,
            });
        }
        Self::check_revision(&wire.descriptor)?;
        let config = config::restore(&wire.descriptor.pipeline, limits, work).await?;
        wire.descriptor
            .runtime_profiles
            .validate_with(&config, work)
            .await?;
        Self::finish_encoded(wire.descriptor, data_bytes, fingerprint, limits, work).await
    }

    fn finish(data: DescriptorData, limits: AnalyzerLimits) -> AnalysisResult<Arc<Self>> {
        crate::work::inline(Self::finish_with(data, limits, &mut ()))
    }

    async fn finish_with(
        data: DescriptorData,
        limits: AnalyzerLimits,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<Arc<Self>> {
        let bytes = output::data(&data, limits, work).await?;
        let fingerprint = AnalyzerFingerprint::digest_with(bytes.as_bytes(), work).await?;
        Self::finish_encoded(data, bytes, fingerprint, limits, work).await
    }

    async fn finish_encoded(
        data: DescriptorData,
        bytes: String,
        fingerprint: AnalyzerFingerprint,
        limits: AnalyzerLimits,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<Arc<Self>> {
        let json = output::wire(&bytes, fingerprint, limits, work).await?;
        Ok(Arc::new(Self {
            data,
            fingerprint,
            // Preserve the existing public raw-JSON carrier and all serializer behavior.
            // Its dependency-owned syntax scan remains an atomic public API call.
            wire: RawValue::from_string(json)?,
        }))
    }

    fn check_revision(data: &DescriptorData) -> AnalysisResult<()> {
        if data.format != FORMAT {
            return Err(invalid("unknown descriptor format"));
        }
        for (component, expected, actual) in [
            ("format", FORMAT_VERSION, data.format_version),
            ("algorithm", ALGORITHM_REVISION, data.algorithm_revision),
            (
                "source mapping",
                SOURCE_MAPPING_REVISION,
                data.source_mapping_revision,
            ),
        ] {
            if expected != actual {
                return Err(AnalysisError::DescriptorRevision {
                    component,
                    expected,
                    actual,
                });
            }
        }
        Ok(())
    }

    pub fn fingerprint(&self) -> AnalyzerFingerprint {
        self.fingerprint
    }
    pub fn length_policy(&self) -> TokenLengthPolicy {
        self.data.length_policy
    }
    pub fn canonical_json(&self) -> &str {
        self.wire.get()
    }

    pub(crate) fn unicode_profile(&self) -> AnalysisResult<crate::unicode::Profile> {
        self.data.runtime_profiles.profile()
    }

    /// Return the resolved configuration for diagnostics or compatibility APIs. Execute a retained compiled handle when exact resource ownership is required.
    pub fn configuration(&self) -> AnalysisResult<Analyzer> {
        crate::work::inline(self.configuration_with(&mut ()))
    }

    pub(crate) async fn configuration_with(
        &self,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<Analyzer> {
        crate::configuration::decode(&self.data.pipeline, work).await
    }

    pub(crate) async fn validated_configuration_with(
        &self,
        limits: AnalyzerLimits,
        work: &mut impl crate::work::Work<AnalysisError>,
    ) -> AnalysisResult<Analyzer> {
        limits::check_limit(
            "analyzer descriptor bytes",
            self.canonical_json().len(),
            limits.max_descriptor_bytes,
        )?;
        let config = self.configuration_with(work).await?;
        config::check_config_with(&config, limits, work).await?;
        Ok(config)
    }

    pub(crate) fn validate_limits(&self, limits: AnalyzerLimits) -> AnalysisResult<()> {
        limits::check_limit(
            "analyzer descriptor bytes",
            self.canonical_json().len(),
            limits.max_descriptor_bytes,
        )?;
        config::check_config(&self.configuration()?, limits)
    }
}

fn invalid(reason: &'static str) -> AnalysisError {
    AnalysisError::Descriptor(reason)
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(array) => Value::Array(array.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}
