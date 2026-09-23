//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Provider adapters retain frozen bindings without exposing mutable access to their containers.

use super::{retained::FieldBindings, AnalyzerBindings};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::{collections::BTreeMap, sync::Arc};

/// Immutable provider metadata. Captures reserve names and ordered-map nodes before copying; clones share the admitted map and original default resolver without allocating.
pub struct RetainedAnalyzerBindings(AnalyzerBindings);

impl RetainedAnalyzerBindings {
    pub fn capture(
        source: &AnalyzerBindings,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        source.retained(control).map(Self)
    }
}

impl Clone for RetainedAnalyzerBindings {
    fn clone(&self) -> Self {
        let fields = match &self.0.fields {
            FieldBindings::Retained(fields) => FieldBindings::Retained(Arc::clone(fields)),
            FieldBindings::Live(fields) => {
                assert!(fields.is_empty(), "captured bindings own frozen fields");
                FieldBindings::Live(BTreeMap::new())
            }
        };
        Self(AnalyzerBindings {
            default: Arc::clone(&self.0.default),
            fields,
        })
    }
}

impl std::ops::Deref for RetainedAnalyzerBindings {
    type Target = AnalyzerBindings;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
