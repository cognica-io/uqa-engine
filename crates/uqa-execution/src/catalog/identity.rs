//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocate nonzero physical identities for durable catalog objects and generations.

pub use uqa_storage::catalog::new_nonzero_catalog_identity;

mod reservation;
pub use reservation::reserve_catalog_oid;

pub fn allocate_catalog_oid(kind: &str) -> Result<i64, uqa_sql::SQLError> {
    loop {
        let mut bytes = [0; 4];
        getrandom::fill(&mut bytes).map_err(|error| {
            uqa_sql::SQLError::Internal(format!("allocate {kind} OID: {error}"))
        })?;
        let oid = u32::from_ne_bytes(bytes);
        if oid >= 16_384 {
            return Ok(i64::from(oid));
        }
    }
}

use uqa_sql::schema::constraint_metadata::{ConstraintMetadataError, ConstraintMetadataResult};

pub fn allocate_catalog_object_id(kind: &str) -> ConstraintMetadataResult<[u8; 16]> {
    let mut object_id = [0_u8; 16];
    getrandom::fill(&mut object_id).map_err(|error| {
        ConstraintMetadataError(format!("allocate {kind} object identity: {error}"))
    })?;
    Ok(object_id)
}
