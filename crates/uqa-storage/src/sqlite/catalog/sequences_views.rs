//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence and SQL view catalog state.

use super::{
    migration_relation, params, Catalog, OptionalExtension, RelationIdentity, RelationKind, Result,
    SQLiteError, SequenceRow, ViewRow,
};

impl Catalog {
    pub fn create_sequence_row(&self, sequence: &SequenceRow) -> Result<bool> {
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let exists = tx
                .query_row(
                    "SELECT 1 FROM _sequences
                      WHERE schema_name = ?1 AND relation_name = ?2",
                    params![sequence.relation.schema, sequence.relation.name],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if exists {
                return Ok(false);
            }
            Self::claim_relation(&tx, &sequence.relation, RelationKind::Sequence)?;
            tx.execute(
                "INSERT INTO _sequences
                    (schema_name, relation_name, kind, start, increment, current, called)
                 VALUES (?1, ?2, 'sequence', ?3, ?4, ?5, ?6)",
                params![
                    sequence.relation.schema,
                    sequence.relation.name,
                    sequence.start,
                    sequence.increment,
                    sequence.current,
                    sequence.called
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn replace_sequence_row(&self, sequence: &SequenceRow) -> Result<bool> {
        self.conn.with(|connection| {
            Ok(connection.execute(
                "UPDATE _sequences
                    SET start = ?3, increment = ?4, current = ?5, called = ?6
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![
                    sequence.relation.schema,
                    sequence.relation.name,
                    sequence.start,
                    sequence.increment,
                    sequence.current,
                    sequence.called
                ],
            )? != 0)
        })
    }

    pub fn drop_sequence_row(&self, name: &str) -> Result<bool> {
        let relation = migration_relation(name)?;
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let removed = tx.execute(
                "DELETE FROM _sequences
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )? != 0;
            if removed {
                Self::release_relation(&tx, &relation, RelationKind::Sequence)?;
            }
            tx.commit()?;
            Ok(removed)
        })
    }

    pub fn load_sequence_rows(&self) -> Result<Vec<SequenceRow>> {
        self.conn.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT schema_name, relation_name, start, increment, current, called
                       FROM _sequences ORDER BY schema_name, relation_name",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(SequenceRow {
                    relation: RelationIdentity::new(
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                    ),
                    start: row.get(2)?,
                    increment: row.get(3)?,
                    current: row.get(4)?,
                    called: row.get(5)?,
                })
            })?;
            let mut sequences = Vec::new();
            for row in rows {
                sequences.push(row?);
            }
            Ok(sequences)
        })
    }

    /// Allocate one sequence value inside `SQLite` itself. `UPDATE RETURNING`
    /// is a single atomic statement, so no engine-side read/modify/write cache
    /// can race another connection.
    pub fn next_sequence_value(&self, name: &str) -> Result<Option<i64>> {
        let relation = migration_relation(name)?;
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let value = tx
                .query_row(
                    "UPDATE _sequences
                        SET current = CASE WHEN called = 0 THEN current
                                           ELSE current + increment END,
                            called = 1
                      WHERE schema_name = ?1 AND relation_name = ?2
                        AND (called = 0
                             OR (increment > 0 AND current <= ?3 - increment)
                             OR (increment < 0 AND current >= ?4 - increment))
                      RETURNING current",
                    params![relation.schema, relation.name, i64::MAX, i64::MIN],
                    |row| row.get(0),
                )
                .optional()?;
            if value.is_none() {
                let exists = tx
                    .query_row(
                        "SELECT 1 FROM _sequences
                          WHERE schema_name = ?1 AND relation_name = ?2",
                        params![relation.schema, relation.name],
                        |_| Ok(()),
                    )
                    .optional()?
                    .is_some();
                if exists {
                    return Err(SQLiteError::StorageBackend(format!(
                        "sequence `{name}` overflow"
                    )));
                }
            }
            tx.commit()?;
            Ok(value)
        })
    }

    pub fn set_sequence_value(&self, name: &str, value: i64) -> Result<Option<i64>> {
        let relation = migration_relation(name)?;
        self.conn.with(|connection| {
            Ok(connection
                .query_row(
                    "UPDATE _sequences SET current = ?3, called = 1
                     WHERE schema_name = ?1 AND relation_name = ?2 RETURNING current",
                    params![relation.schema, relation.name, value],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    pub fn save_view(&self, view: &ViewRow) -> Result<()> {
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            Self::claim_relation(&tx, &view.relation, RelationKind::View)?;
            tx.execute(
                "INSERT OR REPLACE INTO _views
                    (schema_name, relation_name, kind, definition_json)
                 VALUES (?1, ?2, 'view', ?3)",
                params![
                    view.relation.schema,
                    view.relation.name,
                    view.definition_json
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn drop_view(&self, relation: &RelationIdentity) -> Result<bool> {
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let removed = tx.execute(
                "DELETE FROM _views WHERE schema_name = ?1 AND relation_name = ?2",
                params![relation.schema, relation.name],
            )? != 0;
            if removed {
                Self::release_relation(&tx, relation, RelationKind::View)?;
            }
            tx.commit()?;
            Ok(removed)
        })
    }

    pub fn load_views(&self) -> Result<Vec<ViewRow>> {
        self.conn.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT schema_name, relation_name, definition_json
                   FROM _views ORDER BY schema_name, relation_name",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(ViewRow {
                    relation: RelationIdentity::new(
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                    ),
                    definition_json: row.get(2)?,
                })
            })?;
            let mut views = Vec::new();
            for row in rows {
                views.push(row?);
            }
            Ok(views)
        })
    }
}
