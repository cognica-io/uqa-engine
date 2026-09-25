//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::mvcc::StorageTransactionId;
use crate::StorageBackendResult;

/// Persistent data identities supplied by the storage owner; independent of paths and transaction-history restoration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiskANNGeneration {
    pub(super) database: [u8; 16],
    pub(super) table: u64,
    pub(super) index: u64,
    pub(super) generation: u64,
}

impl DiskANNGeneration {
    pub fn new(
        database: [u8; 16],
        table: u64,
        index: u64,
        generation: u64,
    ) -> StorageBackendResult<Self> {
        if database == [0; 16] || table == 0 || index == 0 || generation == 0 {
            return Err(super::invalid(
                "data incarnations and generation must be nonzero",
            ));
        }
        Ok(Self {
            database,
            table,
            index,
            generation,
        })
    }

    pub fn database(self) -> [u8; 16] {
        self.database
    }
    pub fn table(self) -> u64 {
        self.table
    }
    pub fn index(self) -> u64 {
        self.index
    }
    pub fn generation(self) -> u64 {
        self.generation
    }
}

/// Logical vector origin, retained as data after its writer receipt is reclaimed. Revisions are allocated by the vector mutation owner, not inferred from commit time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiskANNVectorVersion {
    pub(super) writer: StorageTransactionId,
    pub(super) revision: u64,
}

impl DiskANNVectorVersion {
    pub fn new(writer: StorageTransactionId, revision: u64) -> StorageBackendResult<Self> {
        if revision == 0 {
            return Err(super::invalid("vector origin revision must be nonzero"));
        }
        Ok(Self { writer, revision })
    }

    pub fn writer(self) -> StorageTransactionId {
        self.writer
    }
    pub fn revision(self) -> u64 {
        self.revision
    }
}
