//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize coalesced projections using the existing native row classes and integer limits.

use super::{
    records::{self, invalid},
    Draft, NativeRead,
};
use crate::inverted_index::encode_index_u64;
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordOwner};
use rusqlite::types::ValueRef;
use uqa_storage::key_value::occurrence_format::OccurrenceAddress as Address;
use uqa_storage::{KeyValueBatch, StorageBackendResult};

pub(super) fn write(
    batch: &mut dyn KeyValueBatch,
    read: &NativeRead,
    owner: NativeRecordOwner,
    draft: &Draft,
) -> StorageBackendResult<()> {
    let address = Address::decode(&draft.address)?;
    let family =
        records::family(address.projection.expect("complete address")).expect("current family");
    let first = draft.values[0]
        .as_deref()
        .ok_or_else(|| invalid("native occurrence row has an incomplete projection"))?;
    let second = || {
        draft.values[1]
            .as_deref()
            .ok_or_else(|| invalid("native occurrence row has an incomplete projection"))
    };
    let table = ValueRef::Text(read.table.as_bytes());
    let field = ValueRef::Text(address.field.unwrap_or_default().as_bytes());
    let doc_id = || {
        encode_index_u64("document", address.document.expect("document address"))
            .map(ValueRef::Integer)
    };
    let row: &[ValueRef<'_>] = match family {
        Family::OccurrenceClusters => &[
            table,
            field,
            ValueRef::Blob(address.term.expect("cluster address")),
            ValueRef::Integer(encode_index_u64(
                "posting cluster",
                address.cluster.expect("cluster address"),
            )?),
            ValueRef::Integer(encode_index_u64(
                "posting count",
                uqa_storage::clustered_postings::score_count_with_control(first, || {
                    read.snapshot.control.check()
                })?,
            )?),
            ValueRef::Blob(first),
            ValueRef::Blob(second()?),
        ],
        Family::OccurrenceDocuments => &[
            table,
            doc_id()?,
            field,
            ValueRef::Blob(first),
            ValueRef::Blob(second()?),
        ],
        Family::OccurrenceLengths => &[table, doc_id()?, field, scalar(first, u64::from_be_bytes)?],
        Family::OccurrenceSkips => &[
            table,
            field,
            ValueRef::Blob(address.term.expect("skip term")),
            doc_id()?,
            scalar(first, u64::from_be_bytes)?,
        ],
        Family::OccurrenceBlockMax => {
            let bound = uqa_storage::key_value::occurrence_format::BlockMaxValue::decode(first)?;
            &[
                table,
                field,
                ValueRef::Blob(address.term.expect("block-max term")),
                ValueRef::Integer(encode_index_u64(
                    "block index",
                    address.ordinal.expect("block index"),
                )?),
                ValueRef::Real(bound.score),
                ValueRef::Text(bound.fingerprint.as_bytes()),
            ]
        }
        Family::OccurrenceFields => {
            if first.len() != 56 {
                return Err(invalid("invalid occurrence field statistics"));
            }
            uqa_storage::inverted_index::IndexedFieldRevision::from_bytes(&first[..40])?;
            &[
                table,
                field,
                ValueRef::Blob(&first[..40]),
                scalar(&first[40..48], u64::from_le_bytes)?,
                scalar(&first[48..], u64::from_le_bytes)?,
            ]
        }
        Family::OccurrenceFormats => &[
            table,
            ValueRef::Text(
                std::str::from_utf8(first)
                    .map_err(|_| invalid("invalid occurrence format text"))?
                    .as_bytes(),
            ),
        ],
        _ => return Err(invalid("legacy occurrence values cannot be written")),
    };
    read.snapshot.put_row(batch, family, owner, row)?;
    Ok(())
}

fn scalar(bytes: &[u8], decode: fn([u8; 8]) -> u64) -> StorageBackendResult<ValueRef<'_>> {
    let value = decode(
        bytes
            .try_into()
            .map_err(|_| invalid("invalid occurrence scalar width"))?,
    );
    Ok(ValueRef::Integer(encode_index_u64(
        "occurrence scalar",
        value,
    )?))
}
