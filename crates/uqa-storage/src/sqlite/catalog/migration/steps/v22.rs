//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog version 22 clustered-posting migration.

use super::super::super::{params, table_exists, Catalog, Result, SQLiteError};
use super::super::{decode_catalog_id, encode_catalog_id};
use crate::clustered_postings::{cluster_id, encode_cluster, encode_terms, ClusterPosting};

fn decode_legacy_positions(blob: &[u8]) -> Result<Vec<u32>> {
    if !blob.len().is_multiple_of(std::mem::size_of::<u32>()) {
        return Err(SQLiteError::StorageBackend(
            "cannot migrate malformed legacy posting positions".into(),
        ));
    }
    Ok(blob
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn insert_migrated_cluster(
    tx: &rusqlite::Transaction<'_>,
    cluster: (String, String, String, u64, Vec<ClusterPosting>),
) -> Result<()> {
    let (table, field, term, cluster_id, postings) = cluster;
    let (score_blob, positions_blob) = encode_cluster(&postings)
        .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
    tx.execute(
        "INSERT INTO _posting_clusters_v22
            (table_name, field, term, cluster_id, posting_count,
             score_blob, positions_blob)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            table,
            field,
            term,
            encode_catalog_id("posting cluster", cluster_id)?,
            encode_catalog_id("posting count", postings.len() as u64)?,
            score_blob,
            positions_blob
        ],
    )?;
    Ok(())
}

fn insert_migrated_document_terms(
    tx: &rusqlite::Transaction<'_>,
    document: (String, i64, String, Vec<String>),
) -> Result<()> {
    let (table, doc_id, field, terms) = document;
    let terms_blob =
        encode_terms(&terms).map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
    tx.execute(
        "INSERT INTO _posting_documents_v22 (table_name, doc_id, field, terms_blob)
         VALUES (?1, ?2, ?3, ?4)",
        params![table, doc_id, field, terms_blob],
    )?;
    Ok(())
}

fn migrate_legacy_clusters_v22(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut statement = tx.prepare(
        "SELECT posting.table_name, posting.field, posting.term,
                posting.doc_id, posting.positions, lengths.length
           FROM _postings AS posting
           LEFT JOIN _doc_lengths AS lengths
             ON lengths.table_name = posting.table_name
            AND lengths.doc_id = posting.doc_id
            AND lengths.field = posting.field
          ORDER BY posting.table_name, posting.field, posting.term,
                   posting.doc_id",
    )?;
    let mut rows = statement.query([])?;
    let mut group: Option<(String, String, String, u64, Vec<ClusterPosting>)> = None;
    while let Some(row) = rows.next()? {
        let table = row.get::<_, String>(0)?;
        let field = row.get::<_, String>(1)?;
        let term = row.get::<_, String>(2)?;
        let stored_doc_id = row.get::<_, i64>(3)?;
        let doc_id = decode_catalog_id("posting document", stored_doc_id)?;
        let positions = decode_legacy_positions(&row.get::<_, Vec<u8>>(4)?)?;
        let stored_length = row.get::<_, Option<i64>>(5)?.ok_or_else(|| {
            SQLiteError::StorageBackend(format!(
                "cannot migrate posting `{table}.{field}.{term}` for document {doc_id}: missing document length"
            ))
        })?;
        let doc_length = decode_catalog_id("posting document length", stored_length)?;
        let next_cluster = cluster_id(doc_id);
        let same_group = group.as_ref().is_some_and(
            |(group_table, group_field, group_term, group_cluster, _)| {
                group_table == &table
                    && group_field == &field
                    && group_term == &term
                    && *group_cluster == next_cluster
            },
        );
        if !same_group {
            if let Some(cluster) = group.take() {
                insert_migrated_cluster(tx, cluster)?;
            }
            group = Some((table, field, term, next_cluster, Vec::new()));
        }
        group
            .as_mut()
            .expect("migration group exists")
            .4
            .push(ClusterPosting {
                doc_id,
                term_freq: positions.len() as u64,
                doc_length,
                positions,
            });
    }
    if let Some(cluster) = group {
        insert_migrated_cluster(tx, cluster)?;
    }
    Ok(())
}

fn migrate_legacy_document_terms_v22(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let mut statement = tx.prepare(
        "SELECT table_name, doc_id, field, term
           FROM _postings
          ORDER BY table_name, doc_id, field, term",
    )?;
    let mut rows = statement.query([])?;
    let mut group: Option<(String, i64, String, Vec<String>)> = None;
    while let Some(row) = rows.next()? {
        let table = row.get::<_, String>(0)?;
        let doc_id = row.get::<_, i64>(1)?;
        decode_catalog_id("posting document", doc_id)?;
        let field = row.get::<_, String>(2)?;
        let term = row.get::<_, String>(3)?;
        let same_group =
            group
                .as_ref()
                .is_some_and(|(group_table, group_doc_id, group_field, _)| {
                    group_table == &table && *group_doc_id == doc_id && group_field == &field
                });
        if !same_group {
            if let Some(document) = group.take() {
                insert_migrated_document_terms(tx, document)?;
            }
            group = Some((table, doc_id, field, Vec::new()));
        }
        group
            .as_mut()
            .expect("migration document group exists")
            .3
            .push(term);
    }
    if let Some(document) = group {
        insert_migrated_document_terms(tx, document)?;
    }
    Ok(())
}

pub(in crate::sqlite::catalog::migration) fn clustered_posting_tables_have_current_shape(
    conn: &rusqlite::Connection,
) -> Result<bool> {
    let posting_clusters = Catalog::table_columns(conn, "_posting_clusters")?;
    let posting_documents = Catalog::table_columns(conn, "_posting_documents")?;
    let posting_clusters_ok = posting_clusters.as_ref().is_some_and(|cols| {
        [
            ("table_name", "TEXT"),
            ("field", "TEXT"),
            ("term", "TEXT"),
            ("cluster_id", "INTEGER"),
            ("posting_count", "INTEGER"),
            ("score_blob", "BLOB"),
            ("positions_blob", "BLOB"),
        ]
        .into_iter()
        .all(|(column, expected)| {
            cols.get(column)
                .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
        })
    });
    let posting_documents_ok = posting_documents.as_ref().is_some_and(|cols| {
        [
            ("table_name", "TEXT"),
            ("doc_id", "INTEGER"),
            ("field", "TEXT"),
            ("terms_blob", "BLOB"),
        ]
        .into_iter()
        .all(|(column, expected)| {
            cols.get(column)
                .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
        })
    });
    Ok(posting_clusters_ok && posting_documents_ok)
}

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let legacy_postings_exist = table_exists(tx, "_postings")?;
    if !legacy_postings_exist && clustered_posting_tables_have_current_shape(tx)? {
        return Ok(());
    }

    tx.execute_batch(
        "
        DROP TABLE IF EXISTS _posting_clusters_v22;
        DROP TABLE IF EXISTS _posting_documents_v22;

        CREATE TABLE _posting_clusters_v22 (
            table_name    TEXT NOT NULL,
            field         TEXT NOT NULL,
            term          TEXT NOT NULL,
            cluster_id    INTEGER NOT NULL,
            posting_count INTEGER NOT NULL CHECK (posting_count > 0),
            score_blob    BLOB NOT NULL,
            positions_blob BLOB NOT NULL,
            PRIMARY KEY (table_name, field, term, cluster_id)
        ) WITHOUT ROWID;

        CREATE TABLE _posting_documents_v22 (
            table_name TEXT NOT NULL,
            doc_id     INTEGER NOT NULL,
            field      TEXT NOT NULL,
            terms_blob BLOB NOT NULL,
            PRIMARY KEY (table_name, doc_id, field)
        ) WITHOUT ROWID;
        ",
    )?;

    if legacy_postings_exist {
        migrate_legacy_clusters_v22(tx)?;
        migrate_legacy_document_terms_v22(tx)?;
    }

    tx.execute_batch(
        "
        DROP TABLE IF EXISTS _postings;
        DROP TABLE IF EXISTS _posting_clusters;
        DROP TABLE IF EXISTS _posting_documents;
        ALTER TABLE _posting_clusters_v22 RENAME TO _posting_clusters;
        ALTER TABLE _posting_documents_v22 RENAME TO _posting_documents;
        ",
    )?;
    Ok(())
}
