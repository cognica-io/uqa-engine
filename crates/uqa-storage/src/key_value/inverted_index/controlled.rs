//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded provider keys, borrowed reads and selected-document graph decoding.

use super::{keys, other_error, FieldStats, IndexedFieldMetadata, KeyValueInvertedIndex};
use crate::clustered_postings::{
    cluster_id, decode_occurrence_document_budgeted, EncodedScoreClusterRef, ScoreClusterVisitor,
};
use crate::key_value::{
    TAG_DOC_LENGTH, TAG_FIELD_STATS, TAG_OCCURRENCE_INDEX, TAG_POSTING,
    TAG_POSTING_CLUSTER_POSITIONS, TAG_POSTING_CLUSTER_SCORE, TAG_POSTING_DOCUMENT,
    TAG_REVERSE_POSTING,
};
use crate::{read_control::StorageReadControl, StorageBackendResult, TokenTermKey};
use keys::encoding::{controlled, Part, Part::Number, Part::Segment};
use uqa_core::{
    memory::{Budgeted, BudgetedVec},
    DocId, IndexStats, TokenOccurrence,
};

impl KeyValueInvertedIndex {
    fn read_key(
        &self,
        kind: u8,
        tail: &[Part<'_>],
        control: &StorageReadControl,
    ) -> StorageBackendResult<BudgetedVec<u8>> {
        controlled(&self.table, TAG_OCCURRENCE_INDEX, Some(kind), tail, control)
    }

    pub(super) fn require_graph_format_budgeted(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let mut marker = false;
        {
            let key = self.read_key(keys::FORMAT, &[], control)?;
            self.store.visit_value(&key, control, &mut |bytes| {
                marker = bytes.is_some();
                if bytes.is_some_and(|bytes| bytes != keys::FORMAT_NAME) {
                    return Err(other_error("unsupported occurrence index format"));
                }
                Ok(())
            })?;
        }
        for tag in [
            TAG_POSTING,
            TAG_POSTING_CLUSTER_SCORE,
            TAG_POSTING_CLUSTER_POSITIONS,
            TAG_POSTING_DOCUMENT,
            TAG_DOC_LENGTH,
            TAG_FIELD_STATS,
            TAG_REVERSE_POSTING,
        ] {
            let prefix = controlled(&self.table, tag, None, &[], control)?;
            if self.store.contains_prefix_budgeted(&prefix, control)? {
                return Err(other_error(
                    "legacy positional data requires an atomic source rebuild",
                ));
            }
        }
        if !marker {
            let prefix = controlled(&self.table, TAG_OCCURRENCE_INDEX, None, &[], control)?;
            if self.store.contains_prefix_budgeted(&prefix, control)? {
                return Err(other_error("occurrence index format marker is missing"));
            }
        }
        control.check()
    }

    pub(super) fn visit_clusters_budgeted(
        &self,
        field: &str,
        term: &TokenTermKey,
        after: Option<u64>,
        limit: usize,
        control: &StorageReadControl,
        visit: &mut ScoreClusterVisitor<'_>,
    ) -> StorageBackendResult<()> {
        self.require_graph_format_budgeted(control)?;
        let prefix = self.read_key(
            keys::SCORE,
            &[Segment(field.as_bytes()), Segment(term.as_bytes())],
            control,
        )?;
        let after = after
            .map(|cluster| {
                self.read_key(
                    keys::SCORE,
                    &[
                        Segment(field.as_bytes()),
                        Segment(term.as_bytes()),
                        Number(cluster),
                    ],
                    control,
                )
            })
            .transpose()?;
        self.store.visit_prefix_after(
            &prefix,
            after.as_deref(),
            limit,
            control,
            &mut |key, bytes| {
                control.check()?;
                let suffix = key
                    .strip_prefix(&*prefix)
                    .ok_or_else(|| other_error("score source returned an unrelated key"))?;
                let cluster_id = u64::from_be_bytes(
                    suffix
                        .try_into()
                        .map_err(|_| other_error("invalid occurrence cluster key"))?,
                );
                visit(EncodedScoreClusterRef {
                    cluster_id,
                    stored_count: None,
                    bytes,
                })
            },
        )
    }

    fn field_stats_budgeted(
        &self,
        field: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<FieldStats>> {
        let key = self.read_key(keys::FIELD, &[Segment(field.as_bytes())], control)?;
        let mut stats = None;
        self.store.visit_value(&key, control, &mut |bytes| {
            stats = bytes.map(FieldStats::from_bytes).transpose()?;
            Ok(())
        })?;
        Ok(stats)
    }

    pub(super) fn scalar_stats_budgeted(
        &self,
        field: &str,
        control: &StorageReadControl,
    ) -> StorageBackendResult<IndexStats> {
        self.require_graph_format_budgeted(control)?;
        let mut stats = IndexStats::default();
        if let Some(field) = self.field_stats_budgeted(field, control)? {
            stats.total_docs = field.doc_count;
            stats.avg_doc_length = field.total_length as f64 / field.doc_count as f64;
        }
        control.check()?;
        Ok(stats)
    }

    pub(super) fn occurrences_budgeted(
        &self,
        doc_id: DocId,
        field: &str,
        term: &TokenTermKey,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Budgeted<Vec<TokenOccurrence>>> {
        self.require_graph_format_budgeted(control)?;
        let cluster = cluster_id(doc_id);
        let mut scores = None;
        {
            let key = self.read_key(
                keys::SCORE,
                &[
                    Segment(field.as_bytes()),
                    Segment(term.as_bytes()),
                    Number(cluster),
                ],
                control,
            )?;
            self.store.visit_value(&key, control, &mut |bytes| {
                if let Some(bytes) = bytes {
                    let mut copy = BudgetedVec::new(control.memory());
                    copy.reserve(bytes.len())?;
                    for (index, byte) in bytes.iter().copied().enumerate() {
                        if index % 1024 == 0 {
                            control.check()?;
                        }
                        copy.push(byte)?;
                    }
                    scores = Some(copy);
                }
                Ok(())
            })?;
        }
        let mut posting = None;
        {
            let key = self.read_key(
                keys::POSITIONS,
                &[
                    Segment(field.as_bytes()),
                    Segment(term.as_bytes()),
                    Number(cluster),
                ],
                control,
            )?;
            self.store.visit_value(&key, control, &mut |positions| {
                match (scores.as_deref(), positions) {
                    (None, None) => {}
                    (Some(scores), Some(positions)) => {
                        posting = decode_occurrence_document_budgeted(
                            cluster,
                            scores,
                            positions,
                            doc_id,
                            control.memory(),
                            || control.check(),
                        )?;
                    }
                    _ => return Err(other_error("occurrence score and graph payloads disagree")),
                }
                Ok(())
            })?;
        }
        drop(scores);
        let Some(posting) = posting else {
            return Ok(Budgeted::new(
                Vec::new(),
                control.memory().empty_reservation(),
            ));
        };
        let mut metadata = None;
        {
            let key = self.read_key(
                keys::METADATA,
                &[Segment(field.as_bytes()), Number(doc_id)],
                control,
            )?;
            self.store.visit_value(&key, control, &mut |bytes| {
                metadata = bytes.map(IndexedFieldMetadata::from_bytes).transpose()?;
                Ok(())
            })?;
        }
        let metadata =
            metadata.ok_or_else(|| other_error("occurrence source metadata is missing"))?;
        let stats = self
            .field_stats_budgeted(field, control)?
            .ok_or_else(|| other_error("indexed field revision is missing"))?;
        if stats.revision != metadata.revision() {
            return Err(other_error("indexed field revisions disagree"));
        }
        metadata.validate_posting(&posting, || control.check())?;
        control.check()?;
        let (posting, memory) = posting.into_parts();
        Ok(Budgeted::new(posting.occurrences, memory))
    }
}
