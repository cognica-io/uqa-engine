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
mod hash;
mod json;
pub(crate) mod limits;
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
        )?
        .descriptor)
    }

    pub(crate) fn resolve_inputs(
        config: &Analyzer,
        length_policy: TokenLengthPolicy,
        limits: AnalyzerLimits,
        #[cfg(feature = "nori")] resources: &crate::nori::NoriResources,
    ) -> AnalysisResult<ResolvedDescriptor> {
        config::check_config(config, limits)?;
        let profiles = RuntimeProfiles::resolve(config)?;
        #[cfg(feature = "nori")]
        let (config, nori) = {
            let mut config = config.clone();
            let nori =
                crate::nori::pipeline::ResolvedNoriPipeline::resolve(&mut config, resources)?;
            (config, nori)
        };
        #[cfg(feature = "nori")]
        let config = &config;
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
        })
    }

    /// Restore resolved inputs without opening any file or substituting current mutable definitions.
    pub fn from_json(json: &str, limits: AnalyzerLimits) -> AnalysisResult<Arc<Self>> {
        limits::check_limit(
            "analyzer descriptor bytes",
            json.len(),
            limits.max_descriptor_bytes,
        )?;
        json::check_unique_keys(json)?;
        let wire: Wire = serde_json::from_str(json)?;
        let data_bytes = limits::encode(
            &canonical(&serde_json::to_value(&wire.descriptor)?),
            limits.max_descriptor_bytes,
            true,
        )?;
        let fingerprint = AnalyzerFingerprint::digest(&data_bytes);
        if wire.fingerprint != fingerprint {
            return Err(AnalysisError::DescriptorFingerprint {
                expected: wire.fingerprint,
                actual: fingerprint,
            });
        }
        Self::check_revision(&wire.descriptor)?;
        let config = config::restore(&wire.descriptor.pipeline, limits)?;
        if RuntimeProfiles::resolve(&config)? != wire.descriptor.runtime_profiles {
            return Err(invalid(
                "runtime Unicode or regular-expression profile differs",
            ));
        }
        Self::finish(wire.descriptor, limits)
    }

    fn finish(data: DescriptorData, limits: AnalyzerLimits) -> AnalysisResult<Arc<Self>> {
        let bytes = limits::encode(
            &canonical(&serde_json::to_value(&data)?),
            limits.max_descriptor_bytes,
            true,
        )?;
        let fingerprint = AnalyzerFingerprint::digest(&bytes);
        let wire = canonical(&serde_json::to_value(Wire {
            descriptor: data.clone(),
            fingerprint,
        })?);
        let bytes = limits::encode(&wire, limits.max_descriptor_bytes, true)?;
        let json = String::from_utf8(bytes).expect("JSON serializer returns UTF-8");
        Ok(Arc::new(Self {
            data,
            fingerprint,
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

    pub(crate) fn configuration(&self) -> AnalysisResult<Analyzer> {
        Ok(serde_json::from_value(self.data.pipeline.clone())?)
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
