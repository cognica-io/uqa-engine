//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native row codecs adapt shared occurrence publication without owning merge algorithms.

use super::records;
use crate::mvcc::native::{
    decode_record, encode_row, NativeRecordFamily as Family, NativeRecordIdentity,
};
use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::inverted_index::IndexedFieldRevision;
use uqa_storage::mvcc::{
    OccurrenceRecordKind as Kind, OccurrenceRecordLayout, OccurrenceRecordValue as Value,
    OccurrenceRelatedKey as Related, VersionError, VersionResult,
};
use uqa_storage::read_control::StorageReadControl;

pub(crate) struct NativeOccurrenceRecords;

fn invalid() -> VersionError {
    VersionError::InvalidEncoding("native occurrence key and value disagree")
}

impl OccurrenceRecordLayout for NativeOccurrenceRecords {
    fn kind(&self, key: &[u8], control: &StorageReadControl) -> VersionResult<Kind> {
        let mut cluster = None;
        let identity =
            NativeRecordIdentity::visit_key_components(key, control, |position, value| {
                if position == 2 {
                    cluster = Some(records::integer(value)?);
                }
                Ok(())
            })?;
        Ok(match identity.family() {
            Family::OccurrenceClusters => Kind::Cluster(cluster.ok_or_else(invalid)?),
            Family::OccurrenceFields => Kind::Statistics,
            Family::OccurrenceFormats => Kind::Format,
            Family::OccurrenceSkips | Family::OccurrenceBlockMax => Kind::Cache,
            _ => return Err(invalid()),
        })
    }

    fn related_key(
        &self,
        key: &[u8],
        related: Related,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        self.kind(key, control)?;
        let owner = NativeRecordIdentity::decode(key)?.owner();
        let family = match related {
            Related::Structure => Family::OccurrenceGuards,
            Related::Format => Family::OccurrenceFormats,
            Related::Skips => Family::OccurrenceSkips,
            Related::BlockMax => Family::OccurrenceBlockMax,
            _ => return Err(invalid()),
        };
        let identity = NativeRecordIdentity::new(family, owner)?;
        match related {
            Related::Structure => identity.encode_key(&[ValueRef::Integer(-1)], control),
            Related::Format => identity.encode_key(&[], control),
            _ => identity.encode_prefix(&[], control),
        }
    }

    fn decode<'a>(
        &self,
        key: &[u8],
        value: &'a [u8],
        control: &StorageReadControl,
    ) -> VersionResult<Value<'a>> {
        let (identity, row) = decode_record(key, value, control)?;
        Ok(match identity.family() {
            Family::OccurrenceClusters => {
                records::project(
                    &row,
                    uqa_storage::key_value::occurrence_format::OccurrenceProjection::Score,
                    control,
                    |_| Ok(()),
                )?;
                Value::Cluster {
                    score: records::blob(row[5])?,
                    positions: records::blob(row[6])?,
                }
            }
            Family::OccurrenceFields => {
                if records::integer(row[3])? == 0 {
                    return Err(invalid());
                }
                Value::Statistics {
                    revision: IndexedFieldRevision::from_bytes(records::blob(row[2])?)?,
                    doc_count: records::integer(row[3])?,
                    total_length: records::integer(row[4])?,
                }
            }
            Family::OccurrenceFormats => {
                Value::Format(row[1].as_str().map_err(|_| invalid())?.as_bytes())
            }
            _ => return Err(invalid()),
        })
    }

    fn encode(
        &self,
        key: &[u8],
        template: &[u8],
        value: Value<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let (identity, row) = decode_record(key, template, control)?;
        let integer = |value: u64| -> VersionResult<ValueRef<'_>> {
            Ok(ValueRef::Integer(
                crate::inverted_index::encode_index_u64("occurrence scalar", value)
                    .map_err(uqa_storage::StorageBackendError::from)?,
            ))
        };
        match (identity.family(), value) {
            (Family::OccurrenceClusters, Value::Cluster { score, positions }) => {
                let count =
                    uqa_storage::clustered_postings::score_count_with_control(score, || {
                        control.check()
                    })?;
                encode_row(
                    &[
                        row[0],
                        row[1],
                        row[2],
                        row[3],
                        integer(count)?,
                        ValueRef::Blob(score),
                        ValueRef::Blob(positions),
                    ],
                    control,
                )
            }
            (
                Family::OccurrenceFields,
                Value::Statistics {
                    revision,
                    doc_count,
                    total_length,
                },
            ) => {
                if doc_count == 0 {
                    return Err(invalid());
                }
                encode_row(
                    &[
                        row[0],
                        row[1],
                        ValueRef::Blob(&revision.to_bytes()?),
                        integer(doc_count)?,
                        integer(total_length)?,
                    ],
                    control,
                )
            }
            (Family::OccurrenceFormats, Value::Format(format)) => {
                encode_row(&[row[0], ValueRef::Text(format)], control)
            }
            _ => Err(invalid()),
        }
    }
}
