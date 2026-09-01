//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    blob_to_positions, cluster_id, decode_u64_value, doc_length_key, encode_cluster, encode_terms,
    key_with_tag, other_error, posting_cluster_positions_key, posting_cluster_score_key,
    posting_document_key, read_str, read_u64, reverse_posting_key, ClusterPosting, DocId,
    FieldName, KeyValueStore, StorageBackendResult, MIGRATION_PAGE_SIZE, TAG_POSTING,
    TAG_REVERSE_POSTING,
};

pub(super) fn migrate_legacy_forward_postings(
    store: &dyn KeyValueStore,
) -> StorageBackendResult<u64> {
    let posting_prefix = key_with_tag(TAG_POSTING);
    let mut after = None::<Vec<u8>>;
    let mut group = None::<(String, String, String, u64, Vec<ClusterPosting>)>;
    let mut posting_count = 0_u64;
    loop {
        let page =
            store.scan_prefix_after(&posting_prefix, after.as_deref(), MIGRATION_PAGE_SIZE)?;
        if page.is_empty() {
            break;
        }
        for (key, value) in page {
            after = Some(key.clone());
            let (table, field, term, doc_id) = decode_legacy_posting_key(&key)?;
            let positions = blob_to_positions(&value)?;
            let doc_length = store
                .get(&doc_length_key(&table, doc_id, &field)?)?
                .map(|value| decode_u64_value(&value))
                .transpose()?
                .ok_or_else(|| {
                    other_error(format!(
                        "cannot migrate posting `{table}.{field}.{term}` for document {doc_id}: missing document length"
                    ))
                })?;
            if !store.contains_key(&reverse_posting_key(&table, doc_id, &field, &term)?)? {
                return Err(other_error(format!(
                    "cannot migrate posting `{table}.{field}.{term}` for document {doc_id}: missing reverse posting"
                )));
            }
            let posting_cluster = cluster_id(doc_id);
            let same_group = group.as_ref().is_some_and(
                |(group_table, group_field, group_term, group_cluster, _)| {
                    group_table == &table
                        && group_field == &field
                        && group_term == &term
                        && *group_cluster == posting_cluster
                },
            );
            if !same_group {
                if let Some(cluster) = group.take() {
                    put_migrated_cluster(store, cluster)?;
                }
                group = Some((table, field, term, posting_cluster, Vec::new()));
            }
            group
                .as_mut()
                .expect("posting migration group exists")
                .4
                .push(ClusterPosting {
                    doc_id,
                    term_freq: positions.len() as u64,
                    doc_length,
                    positions,
                });
            posting_count = posting_count
                .checked_add(1)
                .ok_or_else(|| other_error("legacy posting count overflow"))?;
            store.delete(&key)?;
        }
    }
    if let Some(cluster) = group {
        put_migrated_cluster(store, cluster)?;
    }
    Ok(posting_count)
}

pub(super) fn migrate_legacy_reverse_postings(
    store: &dyn KeyValueStore,
) -> StorageBackendResult<u64> {
    let reverse_prefix = key_with_tag(TAG_REVERSE_POSTING);
    let mut after = None::<Vec<u8>>;
    let mut group = None::<(String, DocId, FieldName, Vec<String>)>;
    let mut reverse_count = 0_u64;
    loop {
        let page =
            store.scan_prefix_after(&reverse_prefix, after.as_deref(), MIGRATION_PAGE_SIZE)?;
        if page.is_empty() {
            break;
        }
        for (key, _) in page {
            after = Some(key.clone());
            let (table, doc_id, field, term) = decode_legacy_reverse_key(&key)?;
            let same_group =
                group
                    .as_ref()
                    .is_some_and(|(group_table, group_doc_id, group_field, _)| {
                        group_table == &table && *group_doc_id == doc_id && group_field == &field
                    });
            if !same_group {
                if let Some(document) = group.take() {
                    put_migrated_document(store, document)?;
                }
                group = Some((table, doc_id, field, Vec::new()));
            }
            group
                .as_mut()
                .expect("reverse posting migration group exists")
                .3
                .push(term);
            reverse_count = reverse_count
                .checked_add(1)
                .ok_or_else(|| other_error("legacy reverse posting count overflow"))?;
            store.delete(&key)?;
        }
    }
    if let Some(document) = group {
        put_migrated_document(store, document)?;
    }
    Ok(reverse_count)
}

fn decode_legacy_posting_key(
    key: &[u8],
) -> StorageBackendResult<(String, FieldName, String, DocId)> {
    let mut offset = 1;
    let table = read_str(key, &mut offset)?;
    let field = read_str(key, &mut offset)?;
    let term = read_str(key, &mut offset)?;
    let doc_id = read_u64(key, &mut offset)?;
    if offset != key.len() {
        return Err(other_error("invalid legacy posting key"));
    }
    Ok((table, field, term, doc_id))
}

fn decode_legacy_reverse_key(
    key: &[u8],
) -> StorageBackendResult<(String, DocId, FieldName, String)> {
    let mut offset = 1;
    let table = read_str(key, &mut offset)?;
    let doc_id = read_u64(key, &mut offset)?;
    let field = read_str(key, &mut offset)?;
    let term = read_str(key, &mut offset)?;
    if offset != key.len() {
        return Err(other_error("invalid legacy reverse posting key"));
    }
    Ok((table, doc_id, field, term))
}

fn put_migrated_cluster(
    store: &dyn KeyValueStore,
    cluster: (String, FieldName, String, u64, Vec<ClusterPosting>),
) -> StorageBackendResult<()> {
    let (table, field, term, posting_cluster, entries) = cluster;
    let (score_blob, positions_blob) = encode_cluster(&entries)?;
    store.put(
        &posting_cluster_score_key(&table, &field, &term, posting_cluster)?,
        &score_blob,
    )?;
    store.put(
        &posting_cluster_positions_key(&table, &field, &term, posting_cluster)?,
        &positions_blob,
    )
}

fn put_migrated_document(
    store: &dyn KeyValueStore,
    document: (String, DocId, FieldName, Vec<String>),
) -> StorageBackendResult<()> {
    let (table, doc_id, field, mut terms) = document;
    // Length-prefixed key segments sort by encoded length before text bytes,
    // while the shared terms codec requires ordinary lexical ordering.
    terms.sort_unstable();
    store.put(
        &posting_document_key(&table, doc_id, &field)?,
        &encode_terms(&terms)?,
    )
}
