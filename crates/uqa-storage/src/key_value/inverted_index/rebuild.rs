//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Publish a complete graph index from original sources in one storage batch.

use super::{
    cluster_id, BTreeMap, ClusterKey, DocId, FieldName, KeyValueInvertedIndex, OccurrencePosting,
    StorageBackendResult,
};

impl KeyValueInvertedIndex {
    pub(super) fn rebuild_documents(
        &mut self,
        documents: Vec<(DocId, BTreeMap<FieldName, String>)>,
    ) -> StorageBackendResult<()> {
        let staged = self.stage_documents(documents, true)?;
        let mut totals = BTreeMap::new();
        let mut clusters = BTreeMap::<ClusterKey, Vec<OccurrencePosting>>::new();
        for (doc_id, fields) in &staged {
            Self::add_field_statistics(&mut totals, fields)?;
            for (field, snapshot) in fields {
                for (term, occurrences) in &snapshot.terms {
                    clusters
                        .entry((field.clone(), term.clone(), cluster_id(*doc_id)))
                        .or_default()
                        .push(OccurrencePosting {
                            doc_id: *doc_id,
                            doc_length: snapshot.metadata.length,
                            occurrences: occurrences.clone(),
                        });
                }
            }
        }
        let mut batch = self.store.batch();
        self.clear_index_batch(batch.as_mut())?;
        for ((field, term, cluster), entries) in clusters {
            self.put_cluster(batch.as_mut(), &field, &term, cluster, &entries)?;
        }
        for (doc_id, fields) in &staged {
            self.put_document(batch.as_mut(), *doc_id, fields)?;
        }
        self.put_field_statistics(batch.as_mut(), totals)?;
        batch.commit()
    }
}
