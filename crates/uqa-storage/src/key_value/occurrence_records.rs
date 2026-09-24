//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common Key/Value occurrence addresses and wire values for MVCC publication.

use super::inverted_index::FieldStats;
use super::occurrence_format::{OccurrenceAddress as Address, OccurrenceProjection as Projection};
use super::occurrence_keys::{
    self as keys,
    encoding::{controlled, Part},
};
use crate::mvcc::{
    OccurrenceRecordKind as Kind, OccurrenceRecordLayout, OccurrenceRecordValue as Value,
    OccurrenceRelatedKey as Related, VersionError, VersionResult,
};
use crate::read_control::StorageReadControl;
use uqa_core::memory::BudgetedVec;

// Keep guards outside the data namespace so drop/recreation cannot reuse a structural boundary.
const GUARDS: u8 = b'z';

pub(crate) fn guard(
    table: &str,
    document: Option<u64>,
    control: &StorageReadControl,
) -> VersionResult<BudgetedVec<u8>> {
    Ok(match document {
        Some(document) => controlled(
            table,
            GUARDS,
            Some(b'd'),
            &[Part::Number(document)],
            control,
        )?,
        None => controlled(table, GUARDS, Some(b's'), &[], control)?,
    })
}

pub(crate) fn format(table: &str, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>> {
    Ok(controlled(
        table,
        super::TAG_OCCURRENCE_INDEX,
        Some(keys::FORMAT),
        &[],
        control,
    )?)
}

fn address(key: &[u8]) -> VersionResult<Address<'_>> {
    let address = Address::decode(key)?;
    if !address.complete() {
        return Err(VersionError::InvalidEncoding(
            "incomplete occurrence replacement",
        ));
    }
    Ok(address)
}

pub struct KeyValueOccurrenceRecords;

impl OccurrenceRecordLayout for KeyValueOccurrenceRecords {
    fn kind(&self, key: &[u8], control: &StorageReadControl) -> VersionResult<Kind> {
        control.cancellation().check()?;
        let address = address(key)?;
        Ok(match address.projection {
            Some(Projection::Score) => Kind::Score(address.cluster.expect("complete cluster")),
            Some(Projection::Positions) => Kind::Positions,
            Some(Projection::Field) => Kind::Statistics,
            Some(Projection::Format) => Kind::Format,
            Some(Projection::Skip | Projection::BlockMax) => Kind::Cache,
            _ => {
                return Err(VersionError::InvalidEncoding(
                    "occurrence merge targets a canonical record",
                ))
            }
        })
    }

    fn related_key(
        &self,
        key: &[u8],
        related: Related,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let mut address = address(key)?;
        match related {
            Related::Structure => guard(address.table, None, control),
            Related::Format => format(address.table, control),
            Related::Score | Related::Positions => {
                address.projection = Some(if matches!(related, Related::Score) {
                    Projection::Score
                } else {
                    Projection::Positions
                });
                if !address.complete() {
                    return Err(VersionError::InvalidEncoding(
                        "invalid occurrence cluster peer",
                    ));
                }
                Ok(address.encode(control)?)
            }
            Related::Skips | Related::BlockMax => Ok(controlled(
                address.table,
                super::TAG_OCCURRENCE_INDEX,
                Some(if matches!(related, Related::Skips) {
                    keys::SKIP
                } else {
                    keys::BLOCK_MAX
                }),
                &[],
                control,
            )?),
        }
    }

    fn decode<'a>(
        &self,
        key: &[u8],
        value: &'a [u8],
        control: &StorageReadControl,
    ) -> VersionResult<Value<'a>> {
        Ok(match self.kind(key, control)? {
            Kind::Score(_) => Value::Score(value),
            Kind::Positions => Value::Positions(value),
            Kind::Statistics => {
                let stats = FieldStats::from_bytes(value)?;
                Value::Statistics {
                    revision: stats.revision,
                    doc_count: stats.doc_count,
                    total_length: stats.total_length,
                }
            }
            Kind::Format => Value::Format(value),
            _ => {
                return Err(VersionError::InvalidEncoding(
                    "invalid occurrence value projection",
                ))
            }
        })
    }

    fn encode(
        &self,
        key: &[u8],
        _template: &[u8],
        value: Value<'_>,
        control: &StorageReadControl,
    ) -> VersionResult<BudgetedVec<u8>> {
        let mut bytes = BudgetedVec::new(control.memory());
        match (self.kind(key, control)?, value) {
            (Kind::Score(_), Value::Score(value))
            | (Kind::Positions, Value::Positions(value))
            | (Kind::Format, Value::Format(value)) => bytes.extend_from_slice(value)?,
            (
                Kind::Statistics,
                Value::Statistics {
                    revision,
                    doc_count,
                    total_length,
                },
            ) => bytes.extend_from_slice(
                &FieldStats {
                    revision,
                    doc_count,
                    total_length,
                }
                .to_bytes()?,
            )?,
            _ => {
                return Err(VersionError::InvalidEncoding(
                    "occurrence key and value disagree",
                ))
            }
        }
        Ok(bytes)
    }
}
