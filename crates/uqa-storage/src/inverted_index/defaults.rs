//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared default compositions for providers that override compound operations at a fixed read boundary.

use super::{
    counter_error, BTreeMap, DocId, IndexStats, InvertedIndex, PostingList, StorageBackendResult,
};

pub fn doc_freq_any_field<I: InvertedIndex + ?Sized>(
    index: &I,
    term: &str,
) -> StorageBackendResult<u64> {
    let mut total = 0_u64;
    for field in index.field_names()? {
        total = total
            .checked_add(index.doc_freq(&field, term)?)
            .ok_or_else(|| counter_error("document frequency"))?;
    }
    Ok(total)
}

pub fn field_stats<I: InvertedIndex + ?Sized>(
    index: &I,
    field: &str,
) -> StorageBackendResult<IndexStats> {
    let mut stats = index.stats()?;
    let field_docs = index.field_doc_count(field)?;
    stats.total_docs = field_docs;
    stats.avg_doc_length = if field_docs > 0 {
        index.total_field_length(field)? as f64 / field_docs as f64
    } else {
        0.0
    };
    Ok(stats)
}

pub fn field_stats_scalar<I: InvertedIndex + ?Sized>(
    index: &I,
    field: &str,
) -> StorageBackendResult<IndexStats> {
    let mut stats = IndexStats::default();
    let field_docs = index.field_doc_count(field)?;
    stats.total_docs = field_docs;
    stats.avg_doc_length = if field_docs > 0 {
        index.total_field_length(field)? as f64 / field_docs as f64
    } else {
        0.0
    };
    Ok(stats)
}

pub fn get_posting_list_any_field<I: InvertedIndex + ?Sized>(
    index: &I,
    term: &str,
) -> StorageBackendResult<PostingList> {
    let mut result = PostingList::new();
    for field in index.field_names()? {
        let pl = index.get_posting_list(&field, term)?;
        result = result.merge_union(&pl);
    }
    Ok(result)
}

pub fn get_term_freqs_bulk<I: InvertedIndex + ?Sized>(
    index: &I,
    doc_ids: &[DocId],
    field: &str,
    term: &str,
) -> StorageBackendResult<BTreeMap<DocId, u64>> {
    let mut out = BTreeMap::new();
    for doc_id in doc_ids {
        out.insert(*doc_id, index.get_term_freq(*doc_id, field, term)?);
    }
    Ok(out)
}

pub fn get_total_doc_length<I: InvertedIndex + ?Sized>(
    index: &I,
    doc_id: DocId,
) -> StorageBackendResult<u64> {
    let mut total = 0_u64;
    for field in index.field_names()? {
        total = total
            .checked_add(index.get_doc_length(doc_id, &field)?)
            .ok_or_else(|| counter_error("document length"))?;
    }
    Ok(total)
}

pub fn get_total_term_freq<I: InvertedIndex + ?Sized>(
    index: &I,
    doc_id: DocId,
    term: &str,
) -> StorageBackendResult<u64> {
    let mut total = 0_u64;
    for field in index.field_names()? {
        total = total
            .checked_add(index.get_term_freq(doc_id, &field, term)?)
            .ok_or_else(|| counter_error("term frequency"))?;
    }
    Ok(total)
}
