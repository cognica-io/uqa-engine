//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    other_error, BTreeMap, BTreeSet, ClusterPosting, DocId, FieldName, KeyValueFieldChanges,
    PostingChange, StorageBackendResult,
};

pub(super) fn merge_cluster_changes(
    entries: Vec<ClusterPosting>,
    changes: BTreeMap<DocId, PostingChange>,
) -> Vec<ClusterPosting> {
    fn push_replacement(
        entries: &mut Vec<ClusterPosting>,
        doc_id: DocId,
        replacement: PostingChange,
    ) {
        if let Some((doc_length, positions)) = replacement {
            entries.push(ClusterPosting {
                doc_id,
                term_freq: positions.len() as u64,
                doc_length,
                positions,
            });
        }
    }

    let mut merged = Vec::with_capacity(entries.len().saturating_add(changes.len()));
    let mut changes = changes.into_iter().peekable();
    for entry in entries {
        while changes
            .peek()
            .is_some_and(|(doc_id, _)| *doc_id < entry.doc_id)
        {
            let (doc_id, replacement) = changes.next().expect("peeked posting change exists");
            push_replacement(&mut merged, doc_id, replacement);
        }
        if changes
            .peek()
            .is_some_and(|(doc_id, _)| *doc_id == entry.doc_id)
        {
            let (doc_id, replacement) = changes.next().expect("peeked posting change exists");
            push_replacement(&mut merged, doc_id, replacement);
        } else {
            merged.push(entry);
        }
    }
    for (doc_id, replacement) in changes {
        push_replacement(&mut merged, doc_id, replacement);
    }
    merged
}

pub(super) fn accumulate_field_changes(
    field_changes: &mut KeyValueFieldChanges,
    old_lengths: &BTreeMap<FieldName, u64>,
    new_lengths: &BTreeMap<FieldName, u64>,
) -> StorageBackendResult<()> {
    let mut affected_fields = BTreeSet::new();
    affected_fields.extend(old_lengths.keys().cloned());
    affected_fields.extend(new_lengths.keys().cloned());
    for field in affected_fields {
        let (old_total, new_total) = field_changes.entry(field.clone()).or_default();
        if let Some(length) = old_lengths.get(&field) {
            *old_total = old_total
                .checked_add(*length)
                .ok_or_else(|| other_error("old field length overflow"))?;
        }
        if let Some(length) = new_lengths.get(&field) {
            *new_total = new_total
                .checked_add(*length)
                .ok_or_else(|| other_error("new field length overflow"))?;
        }
    }
    Ok(())
}
