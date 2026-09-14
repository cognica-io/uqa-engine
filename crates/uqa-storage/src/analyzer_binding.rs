//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable, independently resolved analyzer sides and their field owner.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use uqa_analysis::{AnalyzerResources, CompiledAnalyzer};

use crate::{AnalyzerPhase, InvertedIndex, StorageBackendError, StorageBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerBindingOwner {
    /// The physical field's default or an explicit field assignment.
    Field,
    /// A GIN definition selected the analyzer for both sides.
    Gin,
}

#[derive(Debug, Clone)]
pub struct BoundAnalyzerRevision {
    pub name: Option<String>,
    pub compiled: Arc<CompiledAnalyzer>,
}

#[derive(Debug, Clone)]
pub struct FieldAnalyzerBinding {
    pub index: BoundAnalyzerRevision,
    pub search: BoundAnalyzerRevision,
    pub owner: AnalyzerBindingOwner,
    pub last_phase: AnalyzerPhase,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredSide {
    name: Option<String>,
    descriptor: Box<RawValue>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredBinding {
    format: String,
    version: u32,
    owner: AnalyzerBindingOwner,
    last_phase: AnalyzerPhase,
    index: StoredSide,
    search: StoredSide,
}

impl FieldAnalyzerBinding {
    pub fn unassigned(index: Arc<CompiledAnalyzer>, search: Arc<CompiledAnalyzer>) -> Self {
        Self {
            index: BoundAnalyzerRevision {
                name: None,
                compiled: index,
            },
            search: BoundAnalyzerRevision {
                name: None,
                compiled: search,
            },
            owner: AnalyzerBindingOwner::Field,
            last_phase: AnalyzerPhase::Both,
        }
    }

    pub fn assigned(
        &self,
        name: &str,
        compiled: Arc<CompiledAnalyzer>,
        phase: AnalyzerPhase,
        owner: AnalyzerBindingOwner,
    ) -> Self {
        let mut result = self.clone();
        let revision = BoundAnalyzerRevision {
            name: Some(name.to_owned()),
            compiled,
        };
        match phase {
            AnalyzerPhase::Index => result.index = revision,
            AnalyzerPhase::Search => result.search = revision,
            AnalyzerPhase::Both => {
                result.index = revision.clone();
                result.search = revision;
            }
        }
        result.owner = owner;
        result.last_phase = phase;
        result
    }

    pub fn uses_name(&self, name: &str) -> bool {
        self.index.name.as_deref() == Some(name) || self.search.name.as_deref() == Some(name)
    }

    /// Compatibility label for the most recent explicit assignment. Exact execution always uses both retained revisions.
    pub fn last_assignment(&self) -> Option<(String, String)> {
        let side = match self.last_phase {
            AnalyzerPhase::Index | AnalyzerPhase::Both => &self.index,
            AnalyzerPhase::Search => &self.search,
        };
        side.name
            .clone()
            .map(|name| (name, self.phase_name().to_owned()))
    }

    pub fn phase_name(&self) -> &'static str {
        match self.last_phase {
            AnalyzerPhase::Index => "index",
            AnalyzerPhase::Search => "search",
            AnalyzerPhase::Both => "both",
        }
    }

    /// Install both immutable sides without name or file resolution. Callers provide an empty/restoring provider, or the exact index revision already associated with its postings.
    pub fn install(&self, field: &str, index: &mut dyn InvertedIndex) -> StorageBackendResult<()> {
        self.validate()?;
        index
            .set_field_analyzer_revisions(
                field,
                self.index.compiled.clone(),
                self.search.compiled.clone(),
            )
            .map_err(StorageBackendError::Other)
    }

    pub fn to_json(&self) -> StorageBackendResult<String> {
        self.validate()?;
        let side = |revision: &BoundAnalyzerRevision| -> StorageBackendResult<StoredSide> {
            Ok(StoredSide {
                name: revision.name.clone(),
                descriptor: RawValue::from_string(
                    revision.compiled.descriptor().canonical_json().to_owned(),
                )?,
            })
        };
        Ok(serde_json::to_string(&StoredBinding {
            format: "uqa-field-analyzer-binding".into(),
            version: 1,
            owner: self.owner,
            last_phase: self.last_phase,
            index: side(&self.index)?,
            search: side(&self.search)?,
        })?)
    }

    pub fn from_json(json: &str, resources: &AnalyzerResources) -> StorageBackendResult<Self> {
        // Bound the envelope before allocating either descriptor. RawValue retains duplicate properties for the descriptor's strict verifier.
        let limit = resources.limits().max_descriptor_bytes.saturating_mul(3);
        if json.len() > limit {
            return Err(StorageBackendError::Other(
                "analyzer binding exceeds descriptor limits".into(),
            ));
        }
        let saved: StoredBinding = serde_json::from_str(json)?;
        if saved.format != "uqa-field-analyzer-binding" || saved.version != 1 {
            return Err(StorageBackendError::Other(
                "unsupported analyzer binding format".into(),
            ));
        }
        let side = |saved: StoredSide| -> StorageBackendResult<BoundAnalyzerRevision> {
            Ok(BoundAnalyzerRevision {
                name: saved.name,
                compiled: resources.restore_json(saved.descriptor.get())?,
            })
        };
        let restored = Self {
            owner: saved.owner,
            last_phase: saved.last_phase,
            index: side(saved.index)?,
            search: side(saved.search)?,
        };
        restored.validate()?;
        Ok(restored)
    }

    fn validate(&self) -> StorageBackendResult<()> {
        for side in [&self.index, &self.search] {
            if side
                .name
                .as_deref()
                .is_some_and(|name| name.is_empty() || name.trim() != name)
            {
                return Err(StorageBackendError::Other(
                    "analyzer binding has an invalid name".into(),
                ));
            }
        }
        let invalid_last_assignment = match self.last_phase {
            AnalyzerPhase::Index => self.index.name.is_none(),
            AnalyzerPhase::Search => self.search.name.is_none(),
            AnalyzerPhase::Both => {
                self.index.name != self.search.name
                    || (self.index.name.is_some()
                        && self.index.compiled.descriptor().fingerprint()
                            != self.search.compiled.descriptor().fingerprint())
            }
        };
        if invalid_last_assignment {
            return Err(StorageBackendError::Other(
                "analyzer binding disagrees with its last assignment".into(),
            ));
        }
        if self.owner == AnalyzerBindingOwner::Gin
            && (self.index.name.is_none()
                || self.index.name != self.search.name
                || self.index.compiled.descriptor().fingerprint()
                    != self.search.compiled.descriptor().fingerprint()
                || self.last_phase != AnalyzerPhase::Both)
        {
            return Err(StorageBackendError::Other(
                "GIN analyzer ownership requires one named revision on both sides".into(),
            ));
        }
        Ok(())
    }
}
