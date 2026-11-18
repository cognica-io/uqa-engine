//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::RestoreOptions;
use crate::{sql_registry::Registry, Engine};
use std::{fmt, sync::Arc};

/// Restore limits with application-supplied process-local SQL functions.
#[derive(Clone)]
pub struct RestoreConfiguration {
    pub(super) options: RestoreOptions,
    registry: Option<Arc<Registry>>,
}

impl fmt::Debug for RestoreConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RestoreConfiguration")
            .field("options", &self.options)
            .field("registered_functions", &self.registry.is_some())
            .finish()
    }
}

impl From<RestoreOptions> for RestoreConfiguration {
    fn from(options: RestoreOptions) -> Self {
        Self {
            options,
            registry: None,
        }
    }
}

impl RestoreOptions {
    /// Use the supplied Engine's process-local scalar, table and aggregate registrations for catalog binding and subsequent SQL. The configuration retains only its SQL function registry. A running restore holds registrations stable until it publishes, fails or is dropped.
    pub fn with_registered_functions(self, runtime: &Engine) -> RestoreConfiguration {
        RestoreConfiguration {
            options: self,
            registry: Some(Arc::clone(&runtime.extensions.functions)),
        }
    }
}

impl RestoreConfiguration {
    pub(super) fn admit(self) -> AdmittedRestore {
        AdmittedRestore {
            limits: self.options,
            functions: RestoreFunctions(self.registry.map(|registry| {
                let guard = registry.retain_for_restore();
                (registry, guard)
            })),
        }
    }
}

#[derive(Clone)]
pub(super) struct AdmittedRestore {
    pub(super) limits: RestoreOptions,
    pub(super) functions: RestoreFunctions,
}

#[derive(Clone, Default)]
pub(crate) struct RestoreFunctions(Option<(Arc<Registry>, Arc<()>)>);

impl RestoreFunctions {
    pub(crate) fn attach(&self, candidate: &mut Engine) {
        if let Some((registry, _)) = &self.0 {
            registry.attach(&candidate.session, &candidate.runtime.statement_gate);
            candidate.extensions.functions = Arc::clone(registry);
        }
    }
}
