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

#[derive(Debug, Clone)]
struct Revision {
    compiled: Arc<CompiledAnalyzer>,
    configuration: Arc<Analyzer>,
}

impl Revision {
    fn new(compiled: Arc<CompiledAnalyzer>) -> AnalysisResult<Self> {
        Ok(Self {
            configuration: Arc::new(compiled.descriptor().configuration()?),
            compiled,
        })
    }
}

struct DefaultRevision {
    configuration: Analyzer,
    resources: AnalyzerResources,
    resolved: OnceLock<Revision>,
}

impl DefaultRevision {
    fn resolve(&self) -> AnalysisResult<&Revision> {
        if let Some(revision) = self.resolved.get() {
            return Ok(revision);
        }
        // Resource callbacks run without a binding lock. Concurrent resolution publishes one immutable winner.
        let revision = Revision::new(self.resources.compile(&self.configuration)?)?;
        let _ = self.resolved.set(revision);
        Ok(self.resolved.get().expect("resolved default was published"))
    }

    fn configuration(&self) -> &Analyzer {
        self.resolved
            .get()
            .map_or(&self.configuration, |revision| &revision.configuration)
    }
}

#[derive(Debug, Clone)]
struct FieldRevisions {
    index: Revision,
    search: Revision,
}

/// Index and search revisions are independent. Clones retain the same compiled resources while subsequent field assignments are isolated.
///
/// Infallible provider constructors defer the default configuration's validation to its first successful resolution. That default is then fixed, including synonym contents and dictionary identity. Failed compilation publishes no revision. Explicit field assignment always resolves before changing either side.
#[derive(Clone)]
pub struct AnalyzerBindings {
    default: Arc<DefaultRevision>,
    fields: BTreeMap<String, FieldRevisions>,
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
            fields: BTreeMap::new(),
        }
    }

    pub fn default_configuration(&self) -> &Analyzer {
        self.default.configuration()
    }

    pub fn index_configuration(&self, field: &str) -> &Analyzer {
        self.fields.get(field).map_or_else(
            || self.default.configuration(),
            |pair| &pair.index.configuration,
        )
    }

    pub fn search_configuration(&self, field: &str) -> &Analyzer {
        self.fields.get(field).map_or_else(
            || self.default.configuration(),
            |pair| &pair.search.configuration,
        )
    }

    pub fn index_revision(&self, field: &str) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        let revision = match self.fields.get(field) {
            Some(pair) => &pair.index,
            None => self.default.resolve()?,
        };
        Ok(revision.compiled.clone())
    }

    pub fn search_revision(&self, field: &str) -> AnalysisResult<Arc<CompiledAnalyzer>> {
        let revision = match self.fields.get(field) {
            Some(pair) => &pair.search,
            None => self.default.resolve()?,
        };
        Ok(revision.compiled.clone())
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
        let revision = Revision::new(compiled)?;
        let pair = if phase == AnalyzerPhase::Both {
            FieldRevisions {
                index: revision.clone(),
                search: revision,
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
                AnalyzerPhase::Index => pair.index = revision,
                AnalyzerPhase::Search => pair.search = revision,
                AnalyzerPhase::Both => unreachable!("both sides handled above"),
            }
            pair
        };
        self.fields.insert(field.to_owned(), pair);
        Ok(())
    }

    pub fn remove(&mut self, field: &str) {
        self.fields.remove(field);
    }
}
