//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enum function caches belong either to one expression or to the session's type comparison support function.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// The type selected by one enum comparison call, with independent child calls for anonymous row fields.
#[derive(Debug, Default)]
pub struct EnumComparisonState {
    type_oid: AtomicU32,
    fields: OnceLock<Mutex<BTreeMap<usize, Arc<Self>>>>,
}

impl EnumComparisonState {
    pub(super) fn cached_type(&self) -> Option<u32> {
        match self.type_oid.load(Ordering::Relaxed) {
            0 => None,
            oid => Some(oid),
        }
    }

    pub(super) fn remember_type(&self, oid: u32) -> u32 {
        self.type_oid
            .compare_exchange(0, oid, Ordering::Relaxed, Ordering::Relaxed)
            .map_or_else(|existing| existing, |_| oid)
    }

    pub(in crate::expr) fn field(&self, index: usize) -> Arc<Self> {
        Arc::clone(
            self.fields
                .get_or_init(Mutex::default)
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(index)
                .or_default(),
        )
    }
}

/// One comparison-support cache per declared enum type, shared by arrays and records in one session. Transaction rollback and expression disposal do not reset it.
#[derive(Debug, Default)]
pub struct EnumTypeComparisonStates {
    types: Mutex<BTreeMap<u32, Arc<EnumComparisonState>>>,
}

impl EnumTypeComparisonStates {
    pub(in crate::expr) fn get(&self, type_oid: u32) -> Arc<EnumComparisonState> {
        Arc::clone(
            self.types
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entry(type_oid)
                .or_default(),
        )
    }
}
