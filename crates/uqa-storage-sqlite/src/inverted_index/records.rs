//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical and versioned row projections preserve the existing `SQLite` occurrence format.

use super::{decode_index_u64, encode_index_u64};
use crate::mvcc::native::{NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner};
use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::key_value::occurrence_format::{
    OccurrenceAddress as Address, OccurrenceProjection as Projection,
};
use uqa_storage::mvcc::VersionError;
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};
use Projection::{
    BlockMax, Document, Field, Format, LegacyDocument, LegacyField, LegacyLength, LegacyPositions,
    LegacyPosting, LegacyReverse, LegacyScore, Length, Metadata, Positions, Score, Skip,
};

pub(super) const CURRENT: [Projection; 9] = [
    Projection::Skip,
    Projection::BlockMax,
    Projection::Document,
    Projection::Field,
    Projection::Length,
    Projection::Metadata,
    Projection::Positions,
    Projection::Score,
    Projection::Format,
];

pub(super) fn invalid(message: &str) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}

pub(super) fn identity(
    projection: Projection,
    owner: NativeRecordOwner,
) -> StorageBackendResult<NativeRecordIdentity> {
    NativeRecordIdentity::new(
        family(projection).ok_or_else(|| invalid("legacy occurrence family has no values"))?,
        owner,
    )
    .map_err(VersionError::into_storage_error)
}

pub(super) fn key(
    address: Address<'_>,
    owner: NativeRecordOwner,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    if !address.complete() {
        return Err(invalid("occurrence value requires a complete address"));
    }
    identity(address.projection.expect("complete address"), owner)?
        .encode_key(&components(address, control)?, control)
        .map_err(VersionError::into_storage_error)
}

pub(super) fn prefix(
    address: Address<'_>,
    owner: NativeRecordOwner,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    identity(
        address
            .projection
            .ok_or_else(|| invalid("occurrence prefix requires a projection"))?,
        owner,
    )?
    .encode_prefix(&components(address, control)?, control)
    .map_err(VersionError::into_storage_error)
}

pub(super) fn family(projection: Projection) -> Option<Family> {
    Some(match projection {
        Score | Positions => Family::OccurrenceClusters,
        Document | Metadata => Family::OccurrenceDocuments,
        Length => Family::OccurrenceLengths,
        Field => Family::OccurrenceFields,
        Format => Family::OccurrenceFormats,
        Skip => Family::OccurrenceSkips,
        BlockMax => Family::OccurrenceBlockMax,
        LegacyScore | LegacyPositions => Family::PostingClusters,
        LegacyDocument => Family::PostingDocuments,
        LegacyLength => Family::DocLengths,
        LegacyField => Family::FieldStats,
        LegacyPosting | LegacyReverse => return None,
    })
}

pub(super) fn projections(family: Family) -> &'static [Projection] {
    match family {
        Family::OccurrenceClusters => &[Projection::Score, Projection::Positions],
        Family::OccurrenceDocuments => &[Projection::Document, Projection::Metadata],
        Family::OccurrenceLengths => &[Projection::Length],
        Family::OccurrenceFields => &[Projection::Field],
        Family::OccurrenceFormats => &[Projection::Format],
        Family::OccurrenceSkips => &[Projection::Skip],
        Family::OccurrenceBlockMax => &[Projection::BlockMax],
        _ => &[],
    }
}

pub(super) fn components<'a>(
    address: Address<'a>,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<ValueRef<'a>>> {
    let mut parts = BudgetedVec::new(control.memory());
    match address.projection {
        Some(Score | Positions | Skip | BlockMax) => {
            if let Some(field) = address.field {
                parts.push(ValueRef::Text(field.as_bytes()))?;
            }
            if let Some(term) = address.term {
                parts.push(ValueRef::Blob(term))?;
            }
            if let Some(cluster) = match address.projection {
                Some(Skip) => address.document,
                Some(BlockMax) => address.ordinal,
                _ => address.cluster,
            } {
                parts.push(ValueRef::Integer(encode_index_u64(
                    "posting cluster",
                    cluster,
                )?))?;
            }
        }
        Some(Document | Metadata | Length) => {
            if let Some(document) = address.document {
                parts.push(ValueRef::Integer(encode_index_u64("document", document)?))?;
                if let Some(field) = address.field {
                    parts.push(ValueRef::Text(field.as_bytes()))?;
                }
            }
        }
        Some(Field) => {
            if let Some(field) = address.field {
                parts.push(ValueRef::Text(field.as_bytes()))?;
            }
        }
        _ => {}
    }
    Ok(parts)
}

pub(super) fn integer(value: ValueRef<'_>) -> StorageBackendResult<u64> {
    decode_index_u64(
        "occurrence scalar",
        value
            .as_i64()
            .map_err(|_| invalid("native occurrence integer is invalid"))?,
    )
    .map_err(Into::into)
}
pub(super) fn blob(value: ValueRef<'_>) -> StorageBackendResult<&[u8]> {
    value
        .as_blob()
        .map_err(|_| invalid("native occurrence blob is invalid"))
}

pub(super) fn project<T>(
    row: &[ValueRef<'_>],
    projection: Projection,
    control: &StorageReadControl,
    visit: impl FnOnce(&[u8]) -> StorageBackendResult<T>,
) -> StorageBackendResult<T> {
    control.check()?;
    match projection {
        Score | Positions => {
            let score = blob(row[5])?;
            let count = uqa_storage::clustered_postings::score_count_with_control(score, || {
                control.check()
            })?;
            if count != integer(row[4])? {
                return Err(invalid(
                    "stored posting count disagrees with the score payload",
                ));
            }
            visit(blob(row[if projection == Score { 5 } else { 6 }])?)
        }
        Document | Metadata => visit(blob(row[if projection == Document { 3 } else { 4 }])?),
        Length => visit(&integer(row[3])?.to_be_bytes()),
        Skip => visit(&integer(row[4])?.to_be_bytes()),
        BlockMax => {
            let bound = uqa_storage::key_value::occurrence_format::BlockMaxValue {
                score: row[4]
                    .as_f64()
                    .map_err(|_| invalid("invalid block-max score"))?,
                fingerprint: row[5]
                    .as_str()
                    .map_err(|_| invalid("invalid block-max fingerprint"))?,
            }
            .encode(control)?;
            visit(&bound)
        }
        Field => {
            let revision = blob(row[2])?;
            uqa_storage::inverted_index::IndexedFieldRevision::from_bytes(revision)?;
            let mut bytes = [0; 56];
            bytes[..40].copy_from_slice(revision);
            bytes[40..48].copy_from_slice(&integer(row[3])?.to_le_bytes());
            bytes[48..].copy_from_slice(&integer(row[4])?.to_le_bytes());
            visit(&bytes)
        }
        Format => visit(
            row[1]
                .as_str()
                .map_err(|_| invalid("native occurrence format is invalid"))?
                .as_bytes(),
        ),
        _ => Err(invalid("legacy occurrence values require a source rebuild")),
    }
}

pub(super) fn address_from_key(
    key: &[u8],
    table: &str,
    projection: Projection,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    use crate::mvcc::native::NativeRecordIdentity;
    use uqa_storage::mvcc::VersionError;
    let expected =
        family(projection).ok_or_else(|| invalid("legacy occurrence family is absent"))?;
    let mut field = BudgetedVec::new(control.memory());
    let mut term = BudgetedVec::new(control.memory());
    let mut number = None;
    let identity = NativeRecordIdentity::visit_key_components(key, control, |position, value| {
        let result = match expected {
            Family::OccurrenceClusters | Family::OccurrenceSkips | Family::OccurrenceBlockMax => {
                match position {
                    0 => copy_text(value, &mut field),
                    1 => blob(value)
                        .and_then(|bytes| term.extend_from_slice(bytes).map_err(Into::into)),
                    _ => integer(value).map(|value| number = Some(value)),
                }
            }
            Family::OccurrenceDocuments | Family::OccurrenceLengths => match position {
                0 => integer(value).map(|value| number = Some(value)),
                _ => copy_text(value, &mut field),
            },
            Family::OccurrenceFields => copy_text(value, &mut field),
            _ => Ok(()),
        };
        result.map_err(VersionError::Storage)
    })
    .map_err(VersionError::into_storage_error)?;
    if identity.family() != expected {
        return Err(invalid("native occurrence family changed during scan"));
    }
    let field = (projection != Projection::Format)
        .then(|| std::str::from_utf8(&field).map_err(|_| invalid("invalid occurrence field")))
        .transpose()?;
    let term = matches!(
        expected,
        Family::OccurrenceClusters | Family::OccurrenceSkips | Family::OccurrenceBlockMax
    )
    .then_some(term.as_ref());
    encode_address(table, projection, field, term, number, control)
}

pub(super) fn address_from_row(
    row: &[ValueRef<'_>],
    table: &str,
    projection: Projection,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    let text = |position: usize| {
        row[position]
            .as_str()
            .map_err(|_| invalid("invalid occurrence field"))
    };
    let (field, term, number) = match projection {
        Score | Positions | Skip | BlockMax => {
            (Some(text(1)?), Some(blob(row[2])?), Some(integer(row[3])?))
        }
        Document | Metadata | Length => (Some(text(2)?), None, Some(integer(row[1])?)),
        Field => (Some(text(1)?), None, None),
        Format => (None, None, None),
        _ => return Err(invalid("legacy occurrence rows have no current address")),
    };
    encode_address(table, projection, field, term, number, control)
}

fn encode_address(
    table: &str,
    projection: Projection,
    field: Option<&str>,
    term: Option<&[u8]>,
    number: Option<u64>,
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    let mut address = Address::table(table);
    address.projection = Some(projection);
    address.field = field;
    address.term = term;
    match projection {
        Score | Positions => address.cluster = number,
        BlockMax => address.ordinal = number,
        _ => address.document = number,
    }
    address.encode(control)
}

fn copy_text(value: ValueRef<'_>, output: &mut BudgetedVec<u8>) -> StorageBackendResult<()> {
    output.extend_from_slice(
        value
            .as_str()
            .map_err(|_| invalid("invalid occurrence key text"))?
            .as_bytes(),
    )?;
    Ok(())
}
