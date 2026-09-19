//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed addresses for providers projecting native rows into the common occurrence format.

use super::codec::{other_error, read_segment, read_u64};
use super::occurrence_keys::{
    self as keys,
    encoding::{controlled, Part},
};
use super::{
    TAG_DOC_LENGTH, TAG_FIELD_STATS, TAG_OCCURRENCE_INDEX, TAG_POSTING,
    TAG_POSTING_CLUSTER_POSITIONS, TAG_POSTING_CLUSTER_SCORE, TAG_POSTING_DOCUMENT,
    TAG_REVERSE_POSTING,
};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use uqa_core::memory::BudgetedVec;
use OccurrenceProjection::{
    BlockMax, Document, Field, Format, LegacyDocument, LegacyField, LegacyLength, LegacyPositions,
    LegacyPosting, LegacyReverse, LegacyScore, Length, Metadata, Positions, Score, Skip,
};

mod accelerators;
pub use accelerators::BlockMaxValue;

/// A value projection or a predecessor namespace whose presence requires a source rebuild.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OccurrenceProjection {
    Score,
    Positions,
    Document,
    Length,
    Metadata,
    Field,
    Format,
    Skip,
    BlockMax,
    LegacyPosting,
    LegacyReverse,
    LegacyScore,
    LegacyPositions,
    LegacyDocument,
    LegacyLength,
    LegacyField,
}

impl OccurrenceProjection {
    fn tag(self) -> (u8, Option<u8>) {
        match self {
            Score => (TAG_OCCURRENCE_INDEX, Some(keys::SCORE)),
            Positions => (TAG_OCCURRENCE_INDEX, Some(keys::POSITIONS)),
            Document => (TAG_OCCURRENCE_INDEX, Some(keys::DOCUMENT)),
            Length => (TAG_OCCURRENCE_INDEX, Some(keys::LENGTH)),
            Metadata => (TAG_OCCURRENCE_INDEX, Some(keys::METADATA)),
            Field => (TAG_OCCURRENCE_INDEX, Some(keys::FIELD)),
            Format => (TAG_OCCURRENCE_INDEX, Some(keys::FORMAT)),
            Skip => (TAG_OCCURRENCE_INDEX, Some(keys::SKIP)),
            BlockMax => (TAG_OCCURRENCE_INDEX, Some(keys::BLOCK_MAX)),
            LegacyPosting => (TAG_POSTING, None),
            LegacyReverse => (TAG_REVERSE_POSTING, None),
            LegacyScore => (TAG_POSTING_CLUSTER_SCORE, None),
            LegacyPositions => (TAG_POSTING_CLUSTER_POSITIONS, None),
            LegacyDocument => (TAG_POSTING_DOCUMENT, None),
            LegacyLength => (TAG_DOC_LENGTH, None),
            LegacyField => (TAG_FIELD_STATS, None),
        }
    }
    pub fn is_legacy(self) -> bool {
        self.tag().0 != TAG_OCCURRENCE_INDEX
    }
}

/// A complete value address or a component-aligned prefix. Text and term bytes borrow their encoded key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OccurrenceAddress<'a> {
    pub table: &'a str,
    pub projection: Option<OccurrenceProjection>,
    pub field: Option<&'a str>,
    pub term: Option<&'a [u8]>,
    pub document: Option<u64>,
    pub cluster: Option<u64>,
    pub ordinal: Option<u64>,
}

impl<'a> OccurrenceAddress<'a> {
    pub fn table(table: &'a str) -> Self {
        Self {
            table,
            projection: None,
            field: None,
            term: None,
            document: None,
            cluster: None,
            ordinal: None,
        }
    }

    pub fn decode(key: &'a [u8]) -> StorageBackendResult<Self> {
        let tag = *key
            .first()
            .ok_or_else(|| other_error("missing occurrence key tag"))?;
        let mut offset = 1;
        let table = text(read_segment(key, &mut offset)?)?;
        let mut address = Self::table(table);
        address.projection = match tag {
            TAG_OCCURRENCE_INDEX => match key.get(offset) {
                None => return Ok(address),
                Some(&keys::SCORE) => Some(Score),
                Some(&keys::POSITIONS) => Some(Positions),
                Some(&keys::DOCUMENT) => Some(Document),
                Some(&keys::LENGTH) => Some(Length),
                Some(&keys::METADATA) => Some(Metadata),
                Some(&keys::FIELD) => Some(Field),
                Some(&keys::FORMAT) => Some(Format),
                Some(&keys::SKIP) => Some(Skip),
                Some(&keys::BLOCK_MAX) => Some(BlockMax),
                _ => return Err(other_error("unknown occurrence projection")),
            },
            TAG_POSTING => Some(LegacyPosting),
            TAG_REVERSE_POSTING => Some(LegacyReverse),
            TAG_POSTING_CLUSTER_SCORE => Some(LegacyScore),
            TAG_POSTING_CLUSTER_POSITIONS => Some(LegacyPositions),
            TAG_POSTING_DOCUMENT => Some(LegacyDocument),
            TAG_DOC_LENGTH => Some(LegacyLength),
            TAG_FIELD_STATS => Some(LegacyField),
            _ => return Err(other_error("unknown occurrence namespace")),
        };
        if tag == TAG_OCCURRENCE_INDEX {
            offset += 1;
        }
        if offset == key.len() {
            return Ok(address);
        }
        match address.projection {
            Some(Score | Positions | Skip | BlockMax) => {
                address.field = Some(text(read_segment(key, &mut offset)?)?);
                if offset < key.len() {
                    address.term = Some(read_segment(key, &mut offset)?);
                }
                if offset < key.len() {
                    let number = Some(read_u64(key, &mut offset)?);
                    match address.projection {
                        Some(Skip) => address.document = number,
                        Some(BlockMax) => address.ordinal = number,
                        _ => address.cluster = number,
                    }
                }
            }
            Some(Document | Length) => {
                address.document = Some(read_u64(key, &mut offset)?);
                if offset < key.len() {
                    address.field = Some(text(read_segment(key, &mut offset)?)?);
                }
            }
            Some(Metadata | Field) => {
                address.field = Some(text(read_segment(key, &mut offset)?)?);
                if address.projection == Some(Metadata) && offset < key.len() {
                    address.document = Some(read_u64(key, &mut offset)?);
                }
            }
            _ => return Err(other_error("unexpected occurrence prefix components")),
        }
        if offset != key.len() {
            return Err(other_error("trailing occurrence key bytes"));
        }
        Ok(address)
    }

    pub fn complete(self) -> bool {
        match self.projection {
            Some(Score | Positions) => {
                self.field.is_some() && self.term.is_some() && self.cluster.is_some()
            }
            Some(Skip) => self.field.is_some() && self.term.is_some() && self.document.is_some(),
            Some(BlockMax) => self.field.is_some() && self.term.is_some() && self.ordinal.is_some(),
            Some(Document | Length | Metadata) => self.document.is_some() && self.field.is_some(),
            Some(Field) => self.field.is_some(),
            Some(Format) => true,
            _ => false,
        }
    }

    /// Encode an address or prefix under the caller's allowance. Reject invalid component combinations.
    pub fn encode(self, control: &StorageReadControl) -> StorageBackendResult<BudgetedVec<u8>> {
        let mut parts = [Part::Number(0); 3];
        let mut len = 0;
        let mut push = |part| {
            parts[len] = part;
            len += 1;
        };
        match self.projection {
            Some(Score | Positions | Skip | BlockMax) => {
                if let Some(field) = self.field {
                    push(Part::Segment(field.as_bytes()));
                }
                if let Some(term) = self.term {
                    push(Part::Segment(term));
                }
                if let Some(number) = match self.projection {
                    Some(Skip) => self.document,
                    Some(BlockMax) => self.ordinal,
                    _ => self.cluster,
                } {
                    push(Part::Number(number));
                }
            }
            Some(Document | Length) => {
                if let Some(document) = self.document {
                    push(Part::Number(document));
                }
                if let Some(field) = self.field {
                    push(Part::Segment(field.as_bytes()));
                }
            }
            Some(Metadata | Field) => {
                if let Some(field) = self.field {
                    push(Part::Segment(field.as_bytes()));
                }
                if let Some(document) = self.document {
                    push(Part::Number(document));
                }
            }
            _ => {}
        }
        let (tag, kind) = self
            .projection
            .map_or((TAG_OCCURRENCE_INDEX, None), OccurrenceProjection::tag);
        let encoded = controlled(self.table, tag, kind, &parts[..len], control)?;
        if OccurrenceAddress::decode(&encoded)? != self {
            return Err(other_error("invalid occurrence address components"));
        }
        Ok(encoded)
    }
}

fn text(bytes: &[u8]) -> StorageBackendResult<&str> {
    std::str::from_utf8(bytes).map_err(|_| other_error("occurrence identifier is not UTF-8"))
}

#[cfg(test)]
mod tests;
