//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable analyzer ownership shared by every index provider.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use uqa_analysis::{AnalysisResult, Analyzer, AnalyzerResources, CompiledAnalyzer};

use super::AnalyzerPhase;

mod read_only;
mod retained;
pub use read_only::RetainedAnalyzerBindings;
#[cfg(test)]
mod tests;
use retained::FieldBindings;

struct DefaultRevision {
    configuration: Analyzer,
    resources: AnalyzerResources,
    resolved: OnceLock<Arc<CompiledAnalyzer>>,
}

impl DefaultRevision {
    fn resolve(&self) -> AnalysisResult<&Arc<CompiledAnalyzer>> {
        if let Some(revision) = self.resolved.get() {
            return Ok(revision);
        }
        // Resource callbacks run without a binding lock. Concurrent resolution publishes one immutable winner.
        let revision = self.resources.compile(&self.configuration)?;
        let _ = self.resolved.set(revision);
        Ok(self.resolved.get().expect("resolved default was published"))
    }

    fn configuration(&self) -> &Analyzer {
        self.resolved
            .get()
            .map_or(&self.configuration, |revision| revision.configuration())
    }
}

#[derive(Debug, Clone)]
struct FieldRevisions {
    index: Arc<CompiledAnalyzer>,
    search: Arc<CompiledAnalyzer>,
}

/// The selected default configuration and its lazy, immutable resource resolution. Cloning this handle never copies analyzer diagnostics or reopens resources.
#[derive(Clone)]
pub struct AnalyzerDefault(Arc<DefaultRevision>);

/// Index and search revisions are independent. Clones retain the same compiled resources while subsequent field assignments are isolated.
///
/// Infallible provider constructors defer the default configuration's validation to its first successful resolution. That default is then fixed, including synonym contents and dictionary identity. Failed compilation publishes no revision. Explicit field assignment always resolves before changing either side.
#[derive(Clone)]
pub struct AnalyzerBindings {
    default: Arc<DefaultRevision>,
    fields: FieldBindings,
}

impl std::fmt::Debug for AnalyzerBindings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AnalyzerBindings")
            .field("default", &self.default.configuration())
            .field("fields", &self.fields)
            .finish_non_exhaustive()
    }
}

impl AnalyzerBindings {
    pub fn new(default: Analyzer) -> Self {
        Self::with_resources(default, AnalyzerResources::default())
    }

    pub fn with_resources(default: Analyzer, resources: AnalyzerResources) -> Self {
        Self {
            default: Arc::new(DefaultRevision {
                configuration: default,
                resources,
                resolved: OnceLock::new(),
            }),
            fields: FieldBindings::Live(BTreeMap::new()),
        }
    }

    pub fn default_binding(&self) -> AnalyzerDefault {
        AnalyzerDefault(Arc::clone(&self.default))
    }

    pub fn default_configuration(&self) -> &Analyzer {
        self.default.configuration()
    }

    pub fn index_configuration(&self, field: &str) -> &Analyzer {
        self.fields.get(field).map_or_else(
            || self.default.configuration(),
            |pair| pair.index.configuration(),
        )
    }

    pub fn search_configuration(&self, field: &str) -> &Analyzer {
        self.fields.get(field).map_or_else(
            || self.default.configuration(),
            |pair| pair.search.configuration(),
        )
    }

    pub fn index_revision(&self, field: &str) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        let revision = match self.fields.get(field) {
            Some(pair) => &pair.index,
            None => self.default.resolve()?,
        };
        Ok(Arc::clone(revision))
    }

    pub fn search_revision(&self, field: &str) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        let revision = match self.fields.get(field) {
            Some(pair) => &pair.search,
            None => self.default.resolve()?,
        };
        Ok(Arc::clone(revision))
    }

    pub fn bind(
        &mut self,
        field: &str,
        configuration: &Analyzer,
        phase: AnalyzerPhase,
    ) -> AnalysisResult<()> {
        let compiled = self.default.resources.compile(configuration)?;
        self.bind_revision(field, compiled, phase)
    }

    /// Install an already validated handle without resolving its dictionary aliases or synonym files again.
    pub fn bind_revision(
        &mut self,
        field: &str,
        compiled: Arc<CompiledAnalyzer>,
        phase: AnalyzerPhase,
    ) -> AnalysisResult<()> {
        let pair = if phase == AnalyzerPhase::Both {
            FieldRevisions {
                index: Arc::clone(&compiled),
                search: compiled,
            }
        } else {
            let mut pair = if let Some(pair) = self.fields.get(field) {
                pair.clone()
            } else {
                let default = self.default.resolve()?.clone();
                FieldRevisions {
                    index: default.clone(),
                    search: default,
                }
            };
            match phase {
                AnalyzerPhase::Index => pair.index = compiled,
                AnalyzerPhase::Search => pair.search = compiled,
                AnalyzerPhase::Both => unreachable!("both sides handled above"),
            }
            pair
        };
        self.fields.live_mut().insert(field.to_owned(), pair);
        Ok(())
    }

    pub fn remove(&mut self, field: &str) {
        self.fields.live_mut().remove(field);
    }

    /// Retain both compiled sides and their shared diagnostic configurations without decoding their descriptors.
    pub fn bind_revisions(
        &mut self,
        field: &str,
        index: Arc<CompiledAnalyzer>,
        search: Arc<CompiledAnalyzer>,
    ) -> AnalysisResult<()> {
        let pair = FieldRevisions { index, search };
        self.fields.live_mut().insert(field.to_owned(), pair);
        Ok(())
    }
}
