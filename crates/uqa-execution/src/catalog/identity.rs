//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocate nonzero physical identities for durable catalog objects and generations.

use uqa_storage::{StorageBackendError, StorageBackendResult};

pub fn new_nonzero_catalog_identity(owner: &str, kind: &str) -> StorageBackendResult<[u8; 16]> {
    let mut identity = [0_u8; 16];
    getrandom::fill(&mut identity)
        .map_err(|error| StorageBackendError::Other(format!("allocate {owner} {kind}: {error}")))?;
    if identity == [0; 16] {
        identity[15] = 1;
    }
    Ok(identity)
}

use uqa_sql::schema::constraint_metadata::{ConstraintMetadataError, ConstraintMetadataResult};

pub fn allocate_catalog_object_id(kind: &str) -> ConstraintMetadataResult<[u8; 16]> {
    let mut object_id = [0_u8; 16];
    getrandom::fill(&mut object_id).map_err(|error| {
        ConstraintMetadataError(format!("allocate {kind} object identity: {error}"))
    })?;
    Ok(object_id)
}
