//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RestoreOptions;
use crate::{sql_registry::Registry, Engine};
use std::{fmt, sync::Arc};
use uqa_analysis::AnalyzerResources;

/// Restore limits with application-supplied SQL functions and analyzer resources.
#[derive(Clone)]
pub struct RestoreConfiguration {
    pub(super) options: RestoreOptions,
    registry: Option<Arc<Registry>>,
    analyzers: Option<AnalyzerResources>,
}

impl fmt::Debug for RestoreConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RestoreConfiguration")
            .field("options", &self.options)
            .field("registered_functions", &self.registry.is_some())
            .field("analyzer_resources", &self.analyzers.is_some())
            .finish()
    }
}

impl From<RestoreOptions> for RestoreConfiguration {
    fn from(options: RestoreOptions) -> Self {
        Self {
            options,
            registry: None,
            analyzers: None,
        }
    }
}

impl RestoreOptions {
    /// Use the authorizing runtime's SQL registrations and configured analyzer resources.
    pub fn with_runtime(self, runtime: &Engine) -> RestoreConfiguration {
        let mut configuration = self.with_registered_functions(runtime);
        configuration
            .analyzers
            .clone_from(&runtime.extensions.analyzer_resources);
        configuration
    }

    /// Supply process-local SQL registrations for restore and the resulting writable Engine.
    pub fn with_registered_functions(self, runtime: &Engine) -> RestoreConfiguration {
        RestoreConfiguration::from(self).with_registered_functions(runtime)
    }

    /// Select the installed resources and limits used to decode analyzer descriptors.
    pub fn with_analyzer_resources(self, resources: AnalyzerResources) -> RestoreConfiguration {
        RestoreConfiguration::from(self).with_analyzer_resources(resources)
    }
}

impl RestoreConfiguration {
    /// Retain this Engine's scalar, table and aggregate registrations. A running restore holds them stable until it publishes, fails or is dropped.
    #[must_use]
    pub fn with_registered_functions(mut self, runtime: &Engine) -> Self {
        self.registry = Some(Arc::clone(&runtime.extensions.functions));
        self
    }

    /// Persistent descriptors resolve exact dictionary hashes from this owner. Embedded memory-image resources are decoded from the archive under the supplied limits.
    #[must_use]
    pub fn with_analyzer_resources(mut self, resources: AnalyzerResources) -> Self {
        self.analyzers = Some(resources);
        self
    }

    pub(super) fn admit(self) -> AdmittedRestore {
        AdmittedRestore {
            limits: self.options,
            bindings: RestoreBindings {
                registry: self.registry.map(|registry| {
                    let guard = registry.retain_for_restore();
                    (registry, guard)
                }),
                analyzers: self.analyzers,
            },
        }
    }
}

#[derive(Clone)]
pub(super) struct AdmittedRestore {
    pub(super) limits: RestoreOptions,
    pub(super) bindings: RestoreBindings,
}

#[derive(Clone, Default)]
pub(crate) struct RestoreBindings {
    registry: Option<(Arc<Registry>, Arc<()>)>,
    pub(super) analyzers: Option<AnalyzerResources>,
}

impl RestoreBindings {
    pub(super) fn registered_kinds(&self, name: &str) -> crate::sql_registry::RegisteredKinds {
        self.registry
            .as_ref()
            .map(|(registry, _)| registry.kinds(name))
            .unwrap_or_default()
    }
    pub(crate) fn attach(&self, candidate: &mut Engine) {
        candidate
            .extensions
            .analyzer_resources
            .clone_from(&self.analyzers);
        if let Some((registry, _)) = &self.registry {
            registry.attach(&candidate.session, &candidate.runtime.statement_gate);
            candidate.extensions.functions = Arc::clone(registry);
        }
    }
}
