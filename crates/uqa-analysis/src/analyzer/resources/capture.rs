//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Detached descriptor and dictionary bytes retain no compiled pipelines or resource resolvers.

use super::{AnalyzerResource, AnalyzerResourceKind, CompiledAnalyzer};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ValueRetentionError,
};

struct CapturedDictionary {
    kind: AnalyzerResourceKind,
    sha256: [u8; 32],
    bytes: Vec<u8>,
}

pub struct CapturedAnalyzer {
    descriptor: String,
    resources: Vec<CapturedDictionary>,
}

impl CompiledAnalyzer {
    /// Copy this exact resolved image under independent admission without retaining decoded dictionary models or consulting a resolver.
    pub fn capture_image_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedAnalyzer>, ValueRetentionError> {
        let (descriptor, descriptor_memory) = control
            .copy_text(self.descriptor().canonical_json())?
            .into_parts();
        let mut output = ProductionVec::new(*control);
        output.reserve(self.dictionary_resources().count())?;
        for resource in self.dictionary_resources() {
            let mut bytes = ProductionVec::new(*control);
            bytes.reserve(resource.bytes().len())?;
            bytes.extend_from_slice(resource.bytes())?;
            let (bytes, memory) = bytes.finish()?.into_parts();
            output.push_produced(control.finish(
                CapturedDictionary {
                    kind: resource.kind(),
                    sha256: resource.sha256(),
                    bytes,
                },
                memory,
            )?)?;
        }
        let (resources, resource_memory) = output.finish()?.into_parts();
        control.finish(
            CapturedAnalyzer {
                descriptor,
                resources,
            },
            control.combine(descriptor_memory, resource_memory),
        )
    }
}

impl CapturedAnalyzer {
    pub fn canonical_json(&self) -> &str {
        &self.descriptor
    }

    pub fn dictionary_resources(&self) -> impl ExactSizeIterator<Item = AnalyzerResource<'_>> {
        self.resources.iter().map(|resource| AnalyzerResource {
            kind: resource.kind,
            sha256: resource.sha256,
            bytes: &resource.bytes,
        })
    }
}

#[cfg(test)]
mod tests;
