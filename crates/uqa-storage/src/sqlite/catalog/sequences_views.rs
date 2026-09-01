//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence and SQL view catalog state.

use super::{
    migration_relation, params, Catalog, OptionalExtension, RelationIdentity, RelationKind, Result,
    SQLiteError, SequenceOptions, SequenceRow, ViewRow,
};

fn concrete_sequence_options(sequence: &SequenceRow) -> SequenceOptions {
    let default_min = if sequence.increment > 0 { 1 } else { i64::MIN };
    let default_max = if sequence.increment > 0 { i64::MAX } else { -1 };
    SequenceOptions {
        data_type: sequence.options.data_type.clone(),
        min_value: Some(sequence.options.min_value.unwrap_or(default_min)),
        max_value: Some(sequence.options.max_value.unwrap_or(default_max)),
        cycle: sequence.options.cycle,
    }
}

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
            let options = concrete_sequence_options(sequence);
            tx.execute(
                "INSERT INTO _sequences
                    (schema_name, relation_name, kind, object_id, start, increment, current, called, persistence, data_type, min_value, max_value, cycle)
                 VALUES (?1, ?2, 'sequence', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    sequence.relation.schema,
                    sequence.relation.name,
                    sequence.object_id.as_slice(),
                    sequence.start,
                    sequence.increment,
                    sequence.current,
                    sequence.called,
                    sequence.persistence,
                    options.data_type,
                    options.min_value,
                    options.max_value,
                    options.cycle,
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn replace_sequence_row(&self, sequence: &SequenceRow) -> Result<bool> {
        self.conn.with(|connection| {
            let options = concrete_sequence_options(sequence);
            Ok(connection.execute(
                "UPDATE _sequences
                    SET object_id = ?3, start = ?4, increment = ?5, current = ?6, called = ?7, persistence = ?8,
                        data_type = ?9, min_value = ?10, max_value = ?11, cycle = ?12
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![
                    sequence.relation.schema,
                    sequence.relation.name,
                    sequence.object_id.as_slice(),
                    sequence.start,
                    sequence.increment,
                    sequence.current,
                    sequence.called,
                    sequence.persistence,
                    options.data_type,
                    options.min_value,
                    options.max_value,
                    options.cycle,
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
                "SELECT schema_name, relation_name, object_id, start, increment, current, called, persistence,
                        data_type, min_value, max_value, cycle
                       FROM _sequences ORDER BY schema_name, relation_name",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, bool>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, bool>(11)?,
                ))
            })?;
            let mut sequences = Vec::new();
            for row in rows {
                let (
                    schema,
                    name,
                    object_id,
                    start,
                    increment,
                    current,
                    called,
                    persistence,
                    data_type,
                    min_value,
                    max_value,
                    cycle,
                ) = row?;
                let object_id: [u8; 16] = object_id.try_into().map_err(|value: Vec<u8>| {
                    SQLiteError::StorageBackend(format!(
                        "corrupt sequence `{}.{name}` object identity has {} bytes",
                        schema,
                        value.len()
                    ))
                })?;
                sequences.push(SequenceRow {
                    relation: RelationIdentity::new(schema, name),
                    object_id,
                    start,
                    increment,
                    current,
                    called,
                    persistence,
                    options: SequenceOptions {
                        data_type,
                        min_value: Some(min_value),
                        max_value: Some(max_value),
                        cycle,
                    },
                });
            }
            Ok(sequences)
        })
    }

    /// Allocate one sequence value inside `SQLite` itself. `UPDATE RETURNING`
    /// is a single atomic statement, so no engine-side read/modify/write cache
    /// can race another connection.
    pub fn next_sequence_value(&self, name: &str, object_id: [u8; 16]) -> Result<Option<i64>> {
        let relation = migration_relation(name)?;
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let value = tx
                .query_row(
                    "UPDATE _sequences
                        SET current = CASE
                                WHEN called = 0 THEN current
                                WHEN increment > 0 AND current <= max_value - increment
                                    THEN current + increment
                                WHEN increment < 0 AND current >= min_value - increment
                                    THEN current + increment
                                WHEN cycle != 0 AND increment > 0 THEN min_value
                                WHEN cycle != 0 THEN max_value
                                ELSE current
                            END,
                            called = 1
                      WHERE schema_name = ?1 AND relation_name = ?2
                        AND object_id = ?3
                        AND (called = 0
                             OR (increment > 0 AND current <= max_value - increment)
                             OR (increment < 0 AND current >= min_value - increment)
                             OR cycle != 0)
                      RETURNING current",
                    params![relation.schema, relation.name, object_id.as_slice()],
                    |row| row.get(0),
                )
                .optional()?;
            if value.is_none() {
                let stored_object_id = tx
                    .query_row(
                        "SELECT object_id FROM _sequences
                          WHERE schema_name = ?1 AND relation_name = ?2",
                        params![relation.schema, relation.name],
                        |row| row.get::<_, Vec<u8>>(0),
                    )
                    .optional()?
                    .map(|value| {
                        <[u8; 16]>::try_from(value).map_err(|value: Vec<u8>| {
                            SQLiteError::StorageBackend(format!(
                                "corrupt sequence `{name}` object identity has {} bytes",
                                value.len()
                            ))
                        })
                    })
                    .transpose()?;
                if stored_object_id.is_some_and(|stored| stored == object_id) {
                    return Err(SQLiteError::StorageBackend(format!(
                        "sequence `{name}` exhausted"
                    )));
                }
            }
            tx.commit()?;
            Ok(value)
        })
    }

    pub fn set_sequence_value(
        &self,
        name: &str,
        object_id: [u8; 16],
        value: i64,
        called: bool,
    ) -> Result<Option<i64>> {
        let relation = migration_relation(name)?;
        self.conn.with(|connection| {
            Ok(connection
                .query_row(
                    "UPDATE _sequences SET current = ?4, called = ?5
                     WHERE schema_name = ?1 AND relation_name = ?2 AND object_id = ?3 RETURNING current",
                    params![
                        relation.schema,
                        relation.name,
                        object_id.as_slice(),
                        value,
                        called
                    ],
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
