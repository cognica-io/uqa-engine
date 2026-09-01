//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence and SQL view catalog state.

use super::{
    migration_relation, params, Catalog, OptionalExtension, RelationIdentity, RelationKind, Result,
    SQLiteError, SequenceOptions, SequenceReservationResult, SequenceRow, ViewRow,
};
use crate::catalog::{sequence_value_reservation, SequenceOwner, SequenceOwnerDependency};

fn concrete_sequence_options(sequence: &SequenceRow) -> SequenceOptions {
    let default_min = if sequence.increment > 0 { 1 } else { i64::MIN };
    let default_max = if sequence.increment > 0 { i64::MAX } else { -1 };
    SequenceOptions {
        data_type: sequence.options.data_type.clone(),
        min_value: Some(sequence.options.min_value.unwrap_or(default_min)),
        max_value: Some(sequence.options.max_value.unwrap_or(default_max)),
        cycle: sequence.options.cycle,
        cache_size: sequence.options.cache_size,
    }
}

fn decode_sequence_owner(
    relation: &RelationIdentity,
    table_object_id: Option<Vec<u8>>,
    column_object_id: Option<Vec<u8>>,
    dependency: Option<String>,
) -> Result<Option<SequenceOwner>> {
    let (table_object_id, column_object_id, dependency) =
        match (table_object_id, column_object_id, dependency) {
            (None, None, None) => return Ok(None),
            (Some(table), Some(column), Some(dependency)) => (table, column, dependency),
            _ => {
                return Err(SQLiteError::StorageBackend(format!(
                    "corrupt sequence `{}` has an incomplete owner dependency",
                    relation.qualified_name()
                )))
            }
        };
    let table_object_id: [u8; 16] = table_object_id.try_into().map_err(|value: Vec<u8>| {
        SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` owner table identity has {} bytes",
            relation.qualified_name(),
            value.len()
        ))
    })?;
    let column_object_id: [u8; 16] = column_object_id.try_into().map_err(|value: Vec<u8>| {
        SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` owner column identity has {} bytes",
            relation.qualified_name(),
            value.len()
        ))
    })?;
    let dependency = match dependency.as_str() {
        "a" => SequenceOwnerDependency::Automatic,
        "i" => SequenceOwnerDependency::Internal,
        other => {
            return Err(SQLiteError::StorageBackend(format!(
                "corrupt sequence `{}` owner dependency `{other}`",
                relation.qualified_name()
            )))
        }
    };
    Ok(Some(SequenceOwner {
        table_object_id,
        column_object_id,
        dependency,
    }))
}

fn reserve_sequence_values_in_connection(
    connection: &rusqlite::Connection,
    relation: &RelationIdentity,
    object_id: [u8; 16],
    definition_generation: [u8; 16],
) -> Result<SequenceReservationResult> {
    let stored = connection
        .query_row(
            "SELECT object_id, definition_generation, current, called, increment, min_value, max_value, cycle, cache_size
               FROM _sequences WHERE schema_name = ?1 AND relation_name = ?2",
            params![relation.schema, relation.name],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, bool>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((
        stored_object_id,
        stored_generation,
        current,
        called,
        increment,
        min,
        max,
        cycle,
        cache_size,
    )) = stored
    else {
        return Ok(SequenceReservationResult::Missing);
    };
    let stored_object_id: [u8; 16] = stored_object_id.try_into().map_err(|value: Vec<u8>| {
        SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` object identity has {} bytes",
            relation.qualified_name(),
            value.len()
        ))
    })?;
    if stored_object_id != object_id {
        return Ok(SequenceReservationResult::Missing);
    }
    let stored_generation: [u8; 16] = stored_generation.try_into().map_err(|value: Vec<u8>| {
        SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` definition generation has {} bytes",
            relation.qualified_name(),
            value.len()
        ))
    })?;
    if stored_generation != definition_generation {
        return Ok(SequenceReservationResult::DefinitionChanged);
    }
    if increment == 0 || cache_size <= 0 {
        return Err(SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` has increment {increment} and cache size {cache_size}",
            relation.qualified_name()
        )));
    }
    let Some(reservation) =
        sequence_value_reservation(current, called, increment, min, max, cycle, cache_size)
    else {
        return Ok(SequenceReservationResult::Exhausted);
    };
    let updated = connection.execute(
        "UPDATE _sequences SET current = ?5, called = 1
          WHERE schema_name = ?1 AND relation_name = ?2 AND object_id = ?3 AND definition_generation = ?4",
        params![
            relation.schema,
            relation.name,
            object_id.as_slice(),
            definition_generation.as_slice(),
            reservation.last_value,
        ],
    )?;
    if updated != 1 {
        return Err(SQLiteError::StorageBackend(format!(
            "sequence `{}` changed while reserving cached values",
            relation.qualified_name()
        )));
    }
    Ok(SequenceReservationResult::Reserved(reservation))
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
            let owner_table = sequence.owner.map(|owner| owner.table_object_id);
            let owner_column = sequence.owner.map(|owner| owner.column_object_id);
            let owner_dependency = sequence
                .owner
                .map(|owner| owner.dependency.catalog_code());
            tx.execute(
                "INSERT INTO _sequences
                    (schema_name, relation_name, kind, object_id, definition_generation, start, increment, current, called, persistence, data_type, min_value, max_value, cycle, cache_size, owner_table_object_id, owner_column_object_id, owner_dependency)
                 VALUES (?1, ?2, 'sequence', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    sequence.relation.schema,
                    sequence.relation.name,
                    sequence.object_id.as_slice(),
                    sequence.definition_generation.as_slice(),
                    sequence.start,
                    sequence.increment,
                    sequence.current,
                    sequence.called,
                    sequence.persistence,
                    options.data_type,
                    options.min_value,
                    options.max_value,
                    options.cycle,
                    options.cache_size,
                    owner_table.as_ref().map(<[u8; 16]>::as_slice),
                    owner_column.as_ref().map(<[u8; 16]>::as_slice),
                    owner_dependency,
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn replace_sequence_row(&self, sequence: &SequenceRow) -> Result<bool> {
        self.conn.with(|connection| {
            let options = concrete_sequence_options(sequence);
            let owner_table = sequence.owner.map(|owner| owner.table_object_id);
            let owner_column = sequence.owner.map(|owner| owner.column_object_id);
            let owner_dependency = sequence
                .owner
                .map(|owner| owner.dependency.catalog_code());
            Ok(connection.execute(
                "UPDATE _sequences
                    SET object_id = ?3, definition_generation = ?4, start = ?5, increment = ?6, current = ?7, called = ?8, persistence = ?9,
                        data_type = ?10, min_value = ?11, max_value = ?12, cycle = ?13, cache_size = ?14,
                        owner_table_object_id = ?15, owner_column_object_id = ?16, owner_dependency = ?17
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![
                    sequence.relation.schema,
                    sequence.relation.name,
                    sequence.object_id.as_slice(),
                    sequence.definition_generation.as_slice(),
                    sequence.start,
                    sequence.increment,
                    sequence.current,
                    sequence.called,
                    sequence.persistence,
                    options.data_type,
                    options.min_value,
                    options.max_value,
                    options.cycle,
                    options.cache_size,
                    owner_table.as_ref().map(<[u8; 16]>::as_slice),
                    owner_column.as_ref().map(<[u8; 16]>::as_slice),
                    owner_dependency,
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
                "SELECT schema_name, relation_name, object_id, definition_generation, start, increment, current, called, persistence,
                        data_type, min_value, max_value, cycle, cache_size, owner_table_object_id, owner_column_object_id, owner_dependency
                       FROM _sequences ORDER BY schema_name, relation_name",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, bool>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, bool>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, Option<Vec<u8>>>(14)?,
                    row.get::<_, Option<Vec<u8>>>(15)?,
                    row.get::<_, Option<String>>(16)?,
                ))
            })?;
            let mut sequences = Vec::new();
            for row in rows {
                let (
                    schema,
                    name,
                    object_id,
                    definition_generation,
                    start,
                    increment,
                    current,
                    called,
                    persistence,
                    data_type,
                    min_value,
                    max_value,
                    cycle,
                    cache_size,
                    owner_table_object_id,
                    owner_column_object_id,
                    owner_dependency,
                ) = row?;
                let relation = RelationIdentity::new(schema, name);
                let object_id: [u8; 16] = object_id.try_into().map_err(|value: Vec<u8>| {
                    SQLiteError::StorageBackend(format!(
                        "corrupt sequence `{}` object identity has {} bytes",
                        relation.qualified_name(),
                        value.len()
                    ))
                })?;
                let definition_generation: [u8; 16] = definition_generation
                    .try_into()
                    .map_err(|value: Vec<u8>| {
                        SQLiteError::StorageBackend(format!(
                            "corrupt sequence `{}` definition generation has {} bytes",
                            relation.qualified_name(),
                            value.len()
                        ))
                    })?;
                sequences.push(SequenceRow {
                    owner: decode_sequence_owner(
                        &relation,
                        owner_table_object_id,
                        owner_column_object_id,
                        owner_dependency,
                    )?,
                    relation,
                    object_id,
                    definition_generation,
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
                        cache_size,
                    },
                });
            }
            Ok(sequences)
        })
    }

    pub fn reserve_sequence_values(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> Result<SequenceReservationResult> {
        let relation = migration_relation(name)?;
        self.conn.with_mut(|connection| {
            if connection.is_autocommit() {
                let tx = connection
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let result = reserve_sequence_values_in_connection(
                    &tx,
                    &relation,
                    object_id,
                    definition_generation,
                )?;
                tx.commit()?;
                return Ok(result);
            }
            let tx = connection.savepoint()?;
            let result = reserve_sequence_values_in_connection(
                &tx,
                &relation,
                object_id,
                definition_generation,
            )?;
            tx.commit()?;
            Ok(result)
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
