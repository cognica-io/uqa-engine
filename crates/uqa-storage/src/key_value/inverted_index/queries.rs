//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exact term identities, complete graph reads, and independent score projections.

use super::{
    decode_occurrence_cluster, decode_u64_value, keys, other_error, score_count, BTreeMap,
    BTreeSet, ClusteredPostingCursor, DocId, EncodedScoreCluster, FieldStats, IndexStats,
    KeyValueInvertedIndex, OccurrencePosting, PostingCursor, StorageBackendResult, TokenTermKey,
};

pub(super) fn require_score_version(score: &[u8]) -> StorageBackendResult<()> {
    if score.get(4) != Some(&crate::clustered_postings::OCCURRENCE_FORMAT_VERSION) {
        return Err(other_error(
            "occurrence index contains a legacy score payload",
        ));
    }
    Ok(())
}

impl KeyValueInvertedIndex {
    pub(super) fn score_prefix(&self, field: Option<&str>) -> StorageBackendResult<Vec<u8>> {
        self.require_graph_format()?;
        match field {
            Some(field) => keys::field_prefix(&self.table, keys::SCORE, field),
            None => keys::kind_prefix(&self.table, keys::SCORE),
        }
    }

    pub(super) fn cursor_for_term(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Box<dyn PostingCursor>> {
        self.require_graph_format()?;
        let mut clusters = Vec::new();
        for (key, bytes) in
            self.store
                .scan_prefix(&keys::term_prefix(&self.table, keys::SCORE, field, term)?)?
        {
            let (_, _, cluster_id) = keys::read_cluster(&key, keys::SCORE)?;
            require_score_version(&bytes)?;
            clusters.push(EncodedScoreCluster { cluster_id, bytes });
        }
        Ok(Box::new(ClusteredPostingCursor::new(clusters)?))
    }

    pub(super) fn occurrence_postings(
        &self,
        field: &str,
        term: &TokenTermKey,
    ) -> StorageBackendResult<Vec<OccurrencePosting>> {
        self.require_graph_format()?;
        let mut postings = Vec::new();
        for (key, score) in
            self.store
                .scan_prefix(&keys::term_prefix(&self.table, keys::SCORE, field, term)?)?
        {
            let (_, _, cluster) = keys::read_cluster(&key, keys::SCORE)?;
            let graph = self
                .store
                .get(&keys::cluster_key(
                    &self.table,
                    keys::POSITIONS,
                    field,
                    term,
                    cluster,
                )?)?
                .ok_or_else(|| other_error("occurrence graph payload is missing"))?;
            let entries = decode_occurrence_cluster(cluster, &score, &graph)?;
            for entry in &entries {
                self.validate_posting_metadata(field, entry)?;
            }
            postings.extend(entries);
        }
        Ok(postings)
    }

    pub(super) fn validate_posting_metadata(
        &self,
        field: &str,
        posting: &OccurrencePosting,
    ) -> StorageBackendResult<()> {
        let metadata = self
            .read_field_metadata(posting.doc_id, field)?
            .ok_or_else(|| other_error("occurrence source metadata is missing"))?;
        if posting.doc_length != metadata.length {
            return Err(other_error(
                "occurrence score length disagrees with source metadata",
            ));
        }
        for occurrence in &posting.occurrences {
            if let Some(offsets) = occurrence.offsets {
                if offsets.end_utf8 > metadata.final_offsets.end_utf8
                    || offsets.end_utf16 > metadata.final_offsets.end_utf16
                {
                    return Err(other_error(
                        "occurrence offsets exceed original source bounds",
                    ));
                }
            }
        }
        Ok(())
    }

    pub(super) fn document_length(&self, doc_id: DocId, field: &str) -> StorageBackendResult<u64> {
        self.require_graph_format()?;
        let length = self
            .store
            .get(&keys::document_key(
                &self.table,
                keys::LENGTH,
                doc_id,
                field,
            )?)?
            .map(|value| decode_u64_value(&value))
            .transpose()?;
        let metadata = self.read_field_metadata(doc_id, field)?;
        match (length, metadata) {
            (None, None) => Ok(0),
            (Some(length), Some(metadata)) if length == metadata.length => Ok(length),
            _ => Err(other_error(
                "indexed field length and source metadata disagree",
            )),
        }
    }

    pub(super) fn index_statistics(&self) -> StorageBackendResult<IndexStats> {
        use super::InvertedIndex;
        let doc_count = self.doc_count()?;
        let mut stats = IndexStats::default();
        stats.total_docs = doc_count;
        let mut total = 0_u64;
        for (key, value) in self
            .store
            .scan_prefix(&keys::kind_prefix(&self.table, keys::FIELD)?)?
        {
            keys::read_field(&key)?;
            total = total
                .checked_add(FieldStats::from_bytes(&value)?.total_length)
                .ok_or_else(|| other_error("index total field length overflow"))?;
        }
        if doc_count > 0 {
            stats.avg_doc_length = total as f64 / doc_count as f64;
        }
        let mut counts = BTreeMap::<(String, TokenTermKey), u64>::new();
        for (key, value) in self.store.scan_prefix(&self.score_prefix(None)?)? {
            let (field, term, _) = keys::read_cluster(&key, keys::SCORE)?;
            require_score_version(&value)?;
            let count = counts.entry((field, term)).or_default();
            *count = count
                .checked_add(score_count(&value)?)
                .ok_or_else(|| other_error("index document frequency overflow"))?;
        }
        for ((field, term), frequency) in counts {
            let term = term.to_term();
            if let Some(text) = term.as_str() {
                stats.set_doc_freq(field, text, frequency);
            } else {
                stats.set_doc_freq_utf16(field, term.into_utf16(), frequency);
            }
        }
        Ok(stats)
    }

    pub(super) fn indexed_terms(
        &self,
        field: Option<&str>,
    ) -> StorageBackendResult<Vec<TokenTermKey>> {
        let mut terms = BTreeSet::new();
        for (key, _) in self.store.scan_prefix(&self.score_prefix(field)?)? {
            let (_, term, _) = keys::read_cluster(&key, keys::SCORE)?;
            terms.insert(term);
        }
        Ok(terms.into_iter().collect())
    }
}
