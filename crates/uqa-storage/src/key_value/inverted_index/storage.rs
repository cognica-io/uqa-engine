//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical occurrence projections supply fixed reads and atomic evaluated changes.

use super::{Arc, KeyValueStore, StorageBackendResult};
use crate::key_value::{KeyValueMutation, KeyValueReadScope};

/// Storage for the ordered occurrence format. A provider may project these keys from native rows instead of storing independent byte values. The reader must include the requested table's occurrence namespaces, preserve controlled reads and retained snapshots, and invoke each callback exactly once. Predecessor namespaces require existence checks and atomic retirement during source rebuilds, not value decoding. Mutations publish all projected changes atomically with the original read preconditions.
pub trait OccurrenceStorage: Send + Sync {
    fn read(&self, operation: &mut KeyValueReadScope<'_>) -> StorageBackendResult<()>;
    fn mutate(&self, operation: &mut KeyValueMutation<'_>) -> StorageBackendResult<()>;
}

pub(super) struct KeyValueOccurrences(pub(super) Arc<dyn KeyValueStore>);

impl OccurrenceStorage for KeyValueOccurrences {
    fn read(&self, operation: &mut KeyValueReadScope<'_>) -> StorageBackendResult<()> {
        self.0.with_read_view(operation)
    }
    fn mutate(&self, operation: &mut KeyValueMutation<'_>) -> StorageBackendResult<()> {
        self.0.with_mutation(operation)
    }
}
