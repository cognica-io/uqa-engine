//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analysis keys compare original syntax locations without carrying access to their payloads.

use std::{cmp::Ordering, marker::PhantomData, ptr::NonNull};

/// An opaque location within one live analysis input. The input owner keeps each selected node in place until its observation is consumed; equal syntax at different locations has distinct keys. No pointer can be recovered from this key, and moving the observation does not move or grant access to its input. These keys never enter a published executable or survive the input generation.
#[derive(Debug)]
pub(in crate::binding::preparation) struct AnalysisIdentity<T> {
    address: usize,
    kind: PhantomData<fn() -> T>,
}

impl<T> Copy for AnalysisIdentity<T> {}

impl<T> Clone for AnalysisIdentity<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> From<&T> for AnalysisIdentity<T> {
    fn from(value: &T) -> Self {
        Self::from(NonNull::from(value))
    }
}

impl<T> From<NonNull<T>> for AnalysisIdentity<T> {
    fn from(value: NonNull<T>) -> Self {
        Self {
            address: value.as_ptr().addr(),
            kind: PhantomData,
        }
    }
}

impl<T> PartialEq for AnalysisIdentity<T> {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl<T> Eq for AnalysisIdentity<T> {}

impl<T> PartialOrd for AnalysisIdentity<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for AnalysisIdentity<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.address.cmp(&other.address)
    }
}
