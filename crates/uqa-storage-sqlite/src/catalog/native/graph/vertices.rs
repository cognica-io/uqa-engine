//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Caller-ordered vertex decoding shares one native physical read window. At a snapshot that is the latest commit, an ascending batch reads the vertices' physical projection in one B-tree pass instead of resolving each record's version.

use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::VersionError;

use super::{
    decode_catalog_id, encode_catalog_id, owner, vertex_row, Family, GraphVertexRow,
    NativeSnapshot, Result, ValueRef,
};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};

pub(in crate::catalog) fn for_each_vertex_borrowed(
    snapshot: &NativeSnapshot,
    ids: &[u64],
    visit: &mut dyn FnMut(u64, Option<&GraphVertexRow>) -> bool,
) -> Result<usize> {
    snapshot.control.check()?;
    // An identity outside SQLite's range is reported only when the visitor reaches it, so such a batch keeps the record path.
    let mut encoded = BudgetedVec::new(snapshot.control.memory());
    encoded.reserve(ids.len())?;
    let mut encodable = true;
    for id in ids {
        let Ok(id) = encode_catalog_id("vertex", *id) else {
            encodable = false;
            break;
        };
        encoded.push(id)?;
    }
    if encodable {
        let latest =
            snapshot.visit_latest_vertices(&encoded, &snapshot.control, &mut |id, row| {
                let row = row.map(vertex_row).transpose()?;
                Ok(visit(decode_catalog_id("vertex", id)?, row.as_ref()))
            })?;
        if let Some(count) = latest {
            snapshot.control.check()?;
            return Ok(count);
        }
    }
    drop(encoded);
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
