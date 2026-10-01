//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-ordered vertex decoding shares one native physical read window.

use uqa_storage::mvcc::VersionError;

use super::{
    encode_catalog_id, owner, vertex_row, Family, GraphVertexRow, NativeSnapshot, Result, ValueRef,
};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};

pub(in crate::catalog) fn for_each_vertex_borrowed(
    snapshot: &NativeSnapshot,
    ids: &[u64],
    visit: &mut dyn FnMut(u64, Option<&GraphVertexRow>) -> bool,
) -> Result<usize> {
    snapshot.control.check()?;
    let identity = NativeRecordIdentity::new(Family::GraphVertices, owner(snapshot))?;
    let mut keys = ids.iter().map(|id| {
        let encoded = encode_catalog_id("vertex", *id)
            .map_err(|error| VersionError::Storage(error.into()))?;
        identity.encode_key(&[ValueRef::Integer(encoded)], &snapshot.control)
    });
    let mut count = 0;
    snapshot
        .view
        .visit_values(&mut keys, &snapshot.control, &mut |key, record| {
            let row = record
                .and_then(|record| record.value)
                .map(|bytes| {
                    let (_, row) = decode_record(key, bytes, &snapshot.control)?;
                    vertex_row(&row).map_err(|error| VersionError::Storage(error.into()))
                })
                .transpose()?;
            let more = visit(ids[count], row.as_ref());
            count += 1;
            Ok(more)
        })?;
    snapshot.control.check()?;
    Ok(count)
}
