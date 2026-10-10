//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish a complete graph index from original sources in one storage batch. The documents are analyzed one at a time and staged in a spillable record set before the batch opens, so the batch receives each field record and each cluster once, in the order the index stores them, without every posting in memory.

use super::super::codec::u64_value;
use super::data::{analyze_document, Revisions};
use super::{
    keys, AnalyzerBindings, FieldStats, KeyValueBatch, OccurrenceRead, StorageBackendResult,
};
use crate::inverted_index::{SourceRebuild, TextIndexSource};
use crate::read_control::StorageReadControl;

/// Analyze and stage the documents `source` reads with the revisions `bindings` select, under the session's `control`. A source may read the provider's session, so this runs outside its scopes.
pub(super) fn stage_source(
    bindings: &AnalyzerBindings,
    control: &StorageReadControl,
    source: &mut dyn TextIndexSource,
    cancellation: Option<&uqa_core::CancellationToken>,
) -> StorageBackendResult<SourceRebuild> {
    let mut staged = SourceRebuild::new(control);
    let mut revisions = Revisions::new();
    while let Some((doc_id, fields)) = source.next_document()? {
        let document = analyze_document(bindings, control, fields, &mut revisions, cancellation)?;
        staged.stage(
            doc_id,
            document
                .iter()
                .map(|(field, snapshot)| (field.as_str(), &snapshot.metadata, &snapshot.terms)),
        )?;
    }
    Ok(staged)
}

impl OccurrenceRead<'_> {
    /// Replace the index with `staged` in `batch`.
    pub(super) fn write_rebuild(
        &self,
        batch: &mut dyn KeyValueBatch,
        staged: &SourceRebuild,
        cancellation: Option<&uqa_core::CancellationToken>,
    ) -> StorageBackendResult<()> {
        let check = || cancellation.map_or(Ok(()), uqa_core::CancellationToken::check);
        check()?;
        self.clear_index_batch(batch)?;
        staged.visit_documents(&mut |record| {
            check()?;
            batch.put(
                &keys::metadata_key(self.table, record.field, record.doc_id)?,
                &record.metadata.to_bytes()?,
            )?;
            batch.put(
                &keys::document_key(self.table, keys::LENGTH, record.doc_id, record.field)?,
                &u64_value(record.metadata.length),
            )?;
            batch.put(
                &keys::document_key(self.table, keys::DOCUMENT, record.doc_id, record.field)?,
                record.terms,
            )
        })?;
        staged.visit_clusters(&mut |cluster| {
            check()?;
            // The reset fenced this complete replacement. These are final rows,
            // not document deltas to merge again after another session commits.
            for (kind, value) in [
                (keys::SCORE, cluster.score),
                (keys::POSITIONS, cluster.positions),
            ] {
                batch.put(
                    &keys::cluster_key(
                        self.table,
                        kind,
                        cluster.field,
                        cluster.term,
                        cluster.cluster,
                    )?,
                    value,
                )?;
            }
            Ok(())
        })?;
        for (field, totals) in staged.totals() {
            check()?;
            let stats = FieldStats {
                revision: totals.revision,
                doc_count: totals.doc_count,
                total_length: totals.total_length,
            };
            batch.put(
                &keys::field_prefix(self.table, keys::FIELD, field)?,
                &stats.to_bytes()?,
            )?;
        }
        Ok(check()?)
    }
}
