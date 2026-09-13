//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable source staging and complete persisted document/cluster reads.

use super::{
    analyze_index_field, decode_occurrence_cluster, decode_term_keys, decode_u64_value, keys,
    other_error, AnalyzerBindings, BTreeMap, DocId, DocumentFields, FieldName, FieldSnapshot,
    FieldStats, IndexedFieldMetadata, IndexedFieldRevision, KeyValueInvertedIndex,
    OccurrencePosting, StagedDocuments, StorageBackendResult, TokenTermKey,
};

impl FieldStats {
    pub(super) fn to_bytes(self) -> StorageBackendResult<[u8; 56]> {
        if self.doc_count == 0 {
            return Err(other_error("empty field statistics must be removed"));
        }
        let mut bytes = [0; 56];
        bytes[..40].copy_from_slice(&self.revision.to_bytes()?);
        bytes[40..48].copy_from_slice(&self.doc_count.to_le_bytes());
        bytes[48..].copy_from_slice(&self.total_length.to_le_bytes());
        Ok(bytes)
    }

    pub(super) fn from_bytes(bytes: &[u8]) -> StorageBackendResult<Self> {
        if bytes.len() != 56 {
            return Err(other_error("invalid occurrence field statistics"));
        }
        let stats = Self {
            revision: IndexedFieldRevision::from_bytes(&bytes[..40])?,
            doc_count: u64::from_le_bytes(
                bytes[40..48]
                    .try_into()
                    .map_err(|_| other_error("invalid field document count"))?,
            ),
            total_length: u64::from_le_bytes(
                bytes[48..]
                    .try_into()
                    .map_err(|_| other_error("invalid total field length"))?,
            ),
        };
        stats.to_bytes()?;
        Ok(stats)
    }
}

impl KeyValueInvertedIndex {
    pub(super) fn stored_field_stats(
        &self,
        field: &str,
    ) -> StorageBackendResult<Option<FieldStats>> {
        self.store
            .get(&keys::field_prefix(&self.table, keys::FIELD, field)?)?
            .map(|bytes| FieldStats::from_bytes(&bytes))
            .transpose()
    }

    pub(super) fn validate_index_revision_change(
        &self,
        field: &str,
        candidate: &AnalyzerBindings,
    ) -> StorageBackendResult<()> {
        if self.needs_source_rebuild()? {
            return Ok(());
        }
        if let Some(stored) = self.stored_field_stats(field)? {
            let revision = candidate.index_revision(field)?;
            if stored.revision != IndexedFieldRevision::new(&revision) {
                return Err(other_error(
                    "changing a populated field's index revision requires an atomic source rebuild",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn stage_documents(
        &self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
        rebuilding: bool,
    ) -> StorageBackendResult<StagedDocuments> {
        let mut staged = BTreeMap::new();
        for (doc_id, fields) in documents {
            let mut snapshot = DocumentFields::new();
            for (field, text) in fields {
                if !rebuilding {
                    self.validate_index_revision_change(&field, &self.bindings)?;
                }
                let revision = self.bindings.index_revision(&field)?;
                let analyzed = analyze_index_field(&revision, &text)?;
                let metadata = IndexedFieldMetadata::new(&revision, &analyzed);
                snapshot.insert(
                    field,
                    FieldSnapshot {
                        metadata,
                        terms: analyzed.terms,
                    },
                );
            }
            staged.insert(doc_id, snapshot);
        }
        Ok(staged)
    }

    pub(super) fn old_document(&self, doc_id: DocId) -> StorageBackendResult<DocumentFields> {
        let mut fields = DocumentFields::new();
        for (key, value) in
            self.store
                .scan_prefix(&keys::document_prefix(&self.table, keys::LENGTH, doc_id)?)?
        {
            let (_, field) = keys::read_document(&key, keys::LENGTH)?;
            let metadata = self
                .read_field_metadata(doc_id, &field)?
                .ok_or_else(|| other_error("indexed field end metadata is missing"))?;
            if metadata.length != decode_u64_value(&value)? {
                return Err(other_error(
                    "indexed field length disagrees with its source metadata",
                ));
            }
            let reverse = self
                .store
                .get(&keys::document_key(
                    &self.table,
                    keys::DOCUMENT,
                    doc_id,
                    &field,
                )?)?
                .ok_or_else(|| other_error("indexed field reverse terms are missing"))?;
            fields.insert(
                field,
                FieldSnapshot {
                    metadata,
                    terms: decode_term_keys(&reverse)?
                        .into_iter()
                        .map(|term| (term, Vec::new()))
                        .collect(),
                },
            );
        }
        Ok(fields)
    }

    pub(super) fn read_field_metadata(
        &self,
        doc_id: DocId,
        field: &str,
    ) -> StorageBackendResult<Option<IndexedFieldMetadata>> {
        let Some(bytes) = self
            .store
            .get(&keys::metadata_key(&self.table, field, doc_id)?)?
        else {
            return Ok(None);
        };
        let metadata = IndexedFieldMetadata::from_bytes(&bytes)?;
        let stats = self
            .stored_field_stats(field)?
            .ok_or_else(|| other_error("indexed field revision is missing"))?;
        if stats.revision != metadata.revision() {
            return Err(other_error("indexed field revisions disagree"));
        }
        Ok(Some(metadata))
    }

    pub(super) fn load_cluster(
        &self,
        field: &str,
        term: &TokenTermKey,
        cluster: u64,
    ) -> StorageBackendResult<Vec<OccurrencePosting>> {
        let score = self.store.get(&keys::cluster_key(
            &self.table,
            keys::SCORE,
            field,
            term,
            cluster,
        )?)?;
        let positions = self.store.get(&keys::cluster_key(
            &self.table,
            keys::POSITIONS,
            field,
            term,
            cluster,
        )?)?;
        match (score, positions) {
            (None, None) => Ok(Vec::new()),
            (Some(score), Some(positions)) => {
                decode_occurrence_cluster(cluster, &score, &positions)
            }
            _ => Err(other_error("occurrence score and graph payloads disagree")),
        }
    }
}
