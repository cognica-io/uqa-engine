//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded physical path-index records; no reachability map is loaded at open.

use super::native::graph::paths;
use super::{decode_catalog_id, encode_catalog_id, Catalog, Result, SQLiteError};
use rusqlite::{params, params_from_iter, types::Value as SQLValue, OptionalExtension};
use uqa_storage::MAX_GRAPH_ID_PAGE;

impl Catalog {
    /// The physical graph identity remains usable when reachability data was invalidated.
    pub fn path_index_graph(&self, index: &str) -> Result<Option<String>> {
        if let Some(owner) = self.read_native(|snapshot| paths::recorded_graph(snapshot, index))? {
            return Ok(owner);
        }
        self.conn.with(|c| Self::path_index_graph_on(c, index))
    }

    pub(super) fn path_index_graph_on(
        c: &rusqlite::Connection,
        index: &str,
    ) -> Result<Option<String>> {
        Ok(c.query_row(
            "SELECT graph_name FROM _graph_path_index_state WHERE index_key = ?1",
            [index],
            |row| row.get(0),
        )
        .optional()?)
    }

    pub fn drop_graph_path_indexes(&self, graph: &str) -> Result<()> {
        if self
            .conn
            .with_native_write(|snapshot, batch| {
                super::native::graph::lifecycle::remove_paths(snapshot, batch, graph)
            })?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with_mut(|c| {
            let tx = c.savepoint()?;
            Self::drop_graph_path_indexes_on(&tx, graph)?;
            tx.commit()?;
            Ok(())
        })
    }

    pub(super) fn drop_graph_path_indexes_on(c: &rusqlite::Connection, graph: &str) -> Result<()> {
        let mut next = c.prepare_cached(
            "SELECT graph_name FROM _path_indexes WHERE graph_name > ?1 ORDER BY graph_name LIMIT 1",
        )?;
        let mut exists =
            c.prepare_cached("SELECT EXISTS(SELECT 1 FROM _named_graphs WHERE name = ?1)")?;
        let mut delete = c.prepare_cached("DELETE FROM _path_indexes WHERE graph_name = ?1")?;
        let mut key: Option<String> = c
            .query_row(
                "SELECT graph_name FROM _path_indexes ORDER BY graph_name LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        while let Some(current) = key {
            key = next.query_row([&current], |row| row.get(0)).optional()?;
            if uqa_storage::catalog::path_index::belongs_to(
                &current,
                graph,
                || Self::path_index_graph_on(c, &current).map_err(Into::into),
                |candidate| {
                    exists
                        .query_row([candidate], |row| row.get(0))
                        .map_err(|error| SQLiteError::from(error).into())
                },
            )? {
                delete.execute([&current])?;
            }
        }
        Ok(())
    }

    pub fn clear_path_index_data(&self, index: &str) -> Result<()> {
        if self
            .conn
            .with_native_write(|snapshot, batch| paths::clear(snapshot, batch, index))?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with_mut(|conn| {
            let checkpoint = conn.savepoint()?;
            checkpoint.execute(
                "DELETE FROM _graph_path_pairs WHERE index_key = ?1",
                [index],
            )?;
            checkpoint.execute(
                "DELETE FROM _graph_path_index_state WHERE index_key = ?1",
                [index],
            )?;
            checkpoint.commit()?;
            Ok(())
        })
    }
    pub fn save_path_index_pairs(
        &self,
        index: &str,
        sequence: &str,
        pairs: &[(u64, u64)],
    ) -> Result<()> {
        if pairs.len() > MAX_GRAPH_ID_PAGE {
            return Err(SQLiteError::StorageBackend(
                "path index batch exceeds the bounded page size".into(),
            ));
        }
        let pairs = pairs
            .iter()
            .map(|&(source, target)| {
                Ok((
                    encode_catalog_id("path source", source)?,
                    encode_catalog_id("path target", target)?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        if self
            .conn
            .with_native_write(|snapshot, batch| {
                paths::save_pairs(snapshot, batch, index, sequence, &pairs)
            })?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with_mut(|conn| {
            let checkpoint = conn.savepoint()?;
            {
                let mut statement = checkpoint.prepare_cached("INSERT INTO _graph_path_pairs(index_key, sequence_key, source_id, target_id) VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING")?;
                for (source, target) in pairs { statement.execute(params![index, sequence, source, target])?; }
            }
            checkpoint.commit()?;
            Ok(())
        })
    }
    pub fn finish_path_index_data(&self, index: &str, graph: &str, definition: &str) -> Result<()> {
        if self
            .conn
            .with_native_write(|snapshot, batch| {
                paths::finish(snapshot, batch, index, graph, definition)
            })?
            .is_some()
        {
            return Ok(());
        }
        self.conn.with(|conn| {
            let written = conn.execute("INSERT INTO _graph_path_index_state(index_key, graph_name, definition_json, valid) SELECT ?1, ?2, ?3, 1 WHERE EXISTS(SELECT 1 FROM _path_indexes WHERE graph_name = ?1 AND label_sequences = ?3) ON CONFLICT(index_key) DO UPDATE SET graph_name = excluded.graph_name, definition_json = excluded.definition_json, valid = 1", params![index, graph, definition])?;
            if written == 0 { return Err(SQLiteError::StorageBackend(format!("path index {index:?} definition changed during build"))); }
            Ok(())
        })
    }
    pub fn path_index_data_is_current(&self, index: &str, definition: &str) -> Result<bool> {
        if let Some(current) =
            self.read_native(|snapshot| paths::current(snapshot, index, definition))?
        {
            return Ok(current);
        }
        self.conn.with(|conn| Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM _graph_path_index_state WHERE index_key = ?1 AND definition_json = ?2 AND valid = 1)", params![index, definition], |row| row.get(0))?))
    }
    pub fn path_index_pairs(
        &self,
        index: &str,
        sequence: &str,
        after: Option<(u64, u64)>,
        limit: usize,
    ) -> Result<Vec<(u64, u64)>> {
        uqa_storage::catalog::validate_graph_page(limit)
            .map_err(|error| SQLiteError::StorageBackend(error.to_string()))?;
        if let Some(pairs) =
            self.read_native(|snapshot| paths::pairs(snapshot, index, sequence, after, limit))?
        {
            return Ok(pairs);
        }
        let mut values = vec![
            SQLValue::Text(index.to_owned()),
            SQLValue::Text(sequence.to_owned()),
        ];
        let mut sql = "SELECT source_id, target_id FROM _graph_path_pairs WHERE index_key = ? AND sequence_key = ?".to_owned();
        if let Some((source, target)) = after {
            sql.push_str(" AND (source_id, target_id) > (?, ?)");
            values.push(SQLValue::Integer(encode_catalog_id("path source", source)?));
            values.push(SQLValue::Integer(encode_catalog_id("path target", target)?));
        }
        sql.push_str(" ORDER BY source_id, target_id LIMIT ?");
        values.push(SQLValue::Integer(
            i64::try_from(limit).map_err(|error| SQLiteError::StorageBackend(error.to_string()))?,
        ));
        self.conn.with(|conn| {
            let mut statement = conn.prepare_cached(&sql)?;
            let mut rows = statement.query(params_from_iter(values))?;
            let mut pairs = Vec::new();
            while let Some(row) = rows.next()? {
                pairs.push((
                    decode_catalog_id("path source", row.get(0)?)?,
                    decode_catalog_id("path target", row.get(1)?)?,
                ));
            }
            Ok(pairs)
        })
    }
}
