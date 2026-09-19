//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence and SQL view catalog state.

use super::{
    migration_relation, params, Catalog, OptionalExtension, RelationIdentity, RelationKind, Result,
    SQLiteError, SequenceOptions, SequenceReservationResult, SequenceRow, SequenceSetValueResult,
    ViewRow,
};
use uqa_storage::catalog::{sequence_value_reservation, SequenceValuePosition};

pub(in crate::catalog) mod codec;
use codec::{
    concrete_sequence_options, decode_raw_sequence_row, decode_sequence_identity,
    read_raw_sequence_row,
};

fn with_sequence_value_write<T>(
    connection: &crate::ManagedConnection,
    operation: impl FnOnce(&rusqlite::Connection) -> Result<T>,
) -> Result<T> {
    connection.with_mut(|connection| {
        if connection.is_autocommit() {
            let transaction =
                connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let result = operation(&transaction)?;
            transaction.commit()?;
            return Ok(result);
        }
        let transaction = connection.savepoint()?;
        let result = operation(&transaction)?;
        transaction.commit()?;
        Ok(result)
    })
}

fn reserve_sequence_values_in_connection(
    connection: &rusqlite::Connection,
    relation: &RelationIdentity,
    object_id: [u8; 16],
    definition_generation: [u8; 16],
) -> Result<SequenceReservationResult> {
    let stored = connection
        .query_row(
            "SELECT object_id, definition_generation, current, called, increment, min_value, max_value, cycle, cache_size, log_count
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
                    row.get::<_, i64>(9)?,
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
        log_count,
    )) = stored
    else {
        return Ok(SequenceReservationResult::Missing);
    };
    let stored_object_id = decode_sequence_identity(relation, "object identity", stored_object_id)?;
    if stored_object_id != object_id {
        return Ok(SequenceReservationResult::Missing);
    }
    let stored_generation =
        decode_sequence_identity(relation, "definition generation", stored_generation)?;
    if stored_generation != definition_generation {
        return Ok(SequenceReservationResult::DefinitionChanged);
    }
    if increment == 0 || cache_size <= 0 {
        return Err(SQLiteError::StorageBackend(format!(
            "corrupt sequence `{}` has increment {increment} and cache size {cache_size}",
            relation.qualified_name()
        )));
    }
    let Some(reservation) = sequence_value_reservation(
        SequenceValuePosition {
            current,
            called,
            log_count,
        },
        increment,
        min,
        max,
        cycle,
        cache_size,
    ) else {
        return Ok(SequenceReservationResult::Exhausted);
    };
    let updated = connection.execute(
        "UPDATE _sequences SET current = ?5, called = 1, log_count = ?6
          WHERE schema_name = ?1 AND relation_name = ?2 AND object_id = ?3 AND definition_generation = ?4",
        params![
            relation.schema,
            relation.name,
            object_id.as_slice(),
            definition_generation.as_slice(),
            reservation.last_value,
            reservation.log_count,
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
        if let Some(result) = self.write_native_sequence(sequence, false)? {
            return Ok(result);
        }
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
            let (role_owner, acl_json) = crate::catalog::role_security::encode_sequence(&sequence.security)?;
            tx.execute(
                "INSERT INTO _sequences
                    (schema_name, relation_name, kind, object_id, definition_generation, start, increment, current, called, persistence, data_type, min_value, max_value, cycle, cache_size, owner_table_object_id, owner_column_object_id, owner_dependency, role_owner, acl_json, log_count)
                 VALUES (?1, ?2, 'sequence', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
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
                    role_owner,
                    acl_json,
                    sequence.log_count,
                ],
            )?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn replace_sequence_row(&self, sequence: &SequenceRow) -> Result<bool> {
        if let Some(result) = self.write_native_sequence(sequence, true)? {
            return Ok(result);
        }
        self.conn.with(|connection| {
            let options = concrete_sequence_options(sequence);
            let owner_table = sequence.owner.map(|owner| owner.table_object_id);
            let owner_column = sequence.owner.map(|owner| owner.column_object_id);
            let owner_dependency = sequence
                .owner
                .map(|owner| owner.dependency.catalog_code());
            let (role_owner, acl_json) = crate::catalog::role_security::encode_sequence(&sequence.security)?;
            Ok(connection.execute(
                "UPDATE _sequences
                    SET object_id = ?3, definition_generation = ?4, start = ?5, increment = ?6, current = ?7, called = ?8, persistence = ?9,
                        data_type = ?10, min_value = ?11, max_value = ?12, cycle = ?13, cache_size = ?14,
                        owner_table_object_id = ?15, owner_column_object_id = ?16, owner_dependency = ?17, role_owner = ?18, acl_json = ?19, log_count = ?20
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
                    role_owner,
                    acl_json,
                    sequence.log_count,
                ],
            )? != 0)
        })
    }

    pub fn rename_sequence_row(&self, from: &str, to: &str) -> Result<bool> {
        let from_relation = migration_relation(from)?;
        let to_relation = migration_relation(to)?;
        if let Some(result) = self.rename_native_sequence(&from_relation, &to_relation)? {
            return Ok(result);
        }
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            let source_exists = tx
                .query_row(
                    "SELECT 1 FROM _sequences
                      WHERE schema_name = ?1 AND relation_name = ?2",
                    params![from_relation.schema, from_relation.name],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !source_exists {
                return Ok(false);
            }
            if from_relation == to_relation {
                return Ok(true);
            }
            let target_kind = tx
                .query_row(
                    "SELECT kind FROM _relations
                      WHERE schema_name = ?1 AND relation_name = ?2",
                    params![to_relation.schema, to_relation.name],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(kind) = target_kind {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation `{}` already exists as {kind}",
                    to_relation.qualified_name()
                )));
            }
            Self::claim_relation(&tx, &to_relation, RelationKind::Sequence)?;
            let updated = tx.execute(
                "UPDATE _sequences
                    SET schema_name = ?3, relation_name = ?4
                  WHERE schema_name = ?1 AND relation_name = ?2",
                params![
                    from_relation.schema,
                    from_relation.name,
                    to_relation.schema,
                    to_relation.name
                ],
            )?;
            if updated != 1 {
                return Err(SQLiteError::StorageBackend(format!(
                    "sequence `{from}` changed while renaming"
                )));
            }
            Self::release_relation(&tx, &from_relation, RelationKind::Sequence)?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn drop_sequence_row(&self, name: &str) -> Result<bool> {
        let relation = migration_relation(name)?;
        if let Some(result) = self.drop_native_sequence(&relation)? {
            return Ok(result);
        }
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
        if let Some(result) = self.load_native_sequences()? {
            return Ok(result);
        }
        self.conn.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT schema_name, relation_name, object_id, definition_generation, start, increment, current, called, persistence,
                        data_type, min_value, max_value, cycle, cache_size, owner_table_object_id, owner_column_object_id, owner_dependency, role_owner, acl_json, log_count
                       FROM _sequences ORDER BY schema_name, relation_name",
            )?;
            let sequences = statement
                .query_map([], read_raw_sequence_row)?
                .map(|row| decode_raw_sequence_row(row?))
                .collect();
            sequences
        })
    }

    pub fn reserve_sequence_values(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> Result<SequenceReservationResult> {
        let relation = migration_relation(name)?;
        if let Some(result) =
            self.reserve_native_sequence_values(&relation, object_id, definition_generation)?
        {
            return Ok(result);
        }
        with_sequence_value_write(&self.conn, |connection| {
            reserve_sequence_values_in_connection(
                connection,
                &relation,
                object_id,
                definition_generation,
            )
        })
    }

    pub fn set_sequence_value(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
        value: i64,
        called: bool,
        log_count: i64,
    ) -> Result<SequenceSetValueResult> {
        let relation = migration_relation(name)?;
        if let Some(result) = self.set_native_sequence_value(
            &relation,
            object_id,
            definition_generation,
            value,
            called,
            log_count,
        )? {
            return Ok(result);
        }
        with_sequence_value_write(&self.conn, |connection| {
            let stored = connection
                .query_row(
                    "SELECT object_id, definition_generation FROM _sequences
                  WHERE schema_name = ?1 AND relation_name = ?2",
                    params![relation.schema, relation.name],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?;
            let Some((identity, generation)) = stored else {
                return Ok(SequenceSetValueResult::Missing);
            };
            if decode_sequence_identity(&relation, "object identity", identity)? != object_id {
                return Ok(SequenceSetValueResult::Missing);
            }
            if decode_sequence_identity(&relation, "definition generation", generation)?
                != definition_generation
            {
                return Ok(SequenceSetValueResult::DefinitionChanged);
            }
            let updated = connection.execute(
                "UPDATE _sequences SET current = ?5, called = ?6, log_count = ?7
                  WHERE schema_name = ?1 AND relation_name = ?2 AND object_id = ?3 AND definition_generation = ?4",
                params![relation.schema, relation.name, object_id.as_slice(), definition_generation.as_slice(), value, called, log_count],
            )?;
            if updated != 1 {
                return Err(SQLiteError::StorageBackend(format!(
                    "sequence `{name}` changed while setting its value"
                )));
            }
            Ok(SequenceSetValueResult::Set(value))
        })
    }

    pub fn save_view(&self, view: &ViewRow) -> Result<()> {
        if self.save_native_view(view)?.is_some() {
            return Ok(());
        }
        self.conn.with_mut(|connection| {
            let tx = connection.savepoint()?;
            Self::claim_relation(&tx, &view.relation, RelationKind::View)?;
            let (role_owner, acl_json, column_acls_json) = super::role_security::encode_relation(&view.security)?;
            tx.execute(
                "INSERT OR REPLACE INTO _views
                    (schema_name, relation_name, kind, role_owner, acl_json, column_acls_json, definition_json)
                 VALUES (?1, ?2, 'view', ?3, ?4, ?5, ?6)",
                params![
                    view.relation.schema,
                    view.relation.name,
                    role_owner,
                    acl_json,
                    column_acls_json,
                    view.definition_json
                ],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn rename_view(&self, from: &RelationIdentity, to: &RelationIdentity) -> Result<bool> {
        if from.schema != to.schema {
            return Err(SQLiteError::StorageBackend(
                "moving a view between schemas is not supported by the catalog".into(),
            ));
        }
        if let Some(renamed) =
            self.rename_native_relation(super::native::RelationRecord::View, from, to)?
        {
            return Ok(renamed);
        }
        self.conn.with_mut(|connection| {
            let source_exists = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM _views WHERE schema_name = ?1 AND relation_name = ?2)",
                params![from.schema, from.name],
                |row| row.get::<_, bool>(0),
            )?;
            if from == to || !source_exists {
                return Ok(source_exists);
            }
            let target_exists = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM _relations WHERE schema_name = ?1 AND relation_name = ?2)",
                params![to.schema, to.name],
                |row| row.get::<_, bool>(0),
            )?;
            if target_exists {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation `{}` already exists",
                    to.qualified_name()
                )));
            }
            let tx = connection.savepoint()?;
            Self::claim_relation(&tx, to, RelationKind::View)?;
            let updated = tx.execute(
                "UPDATE _views SET schema_name = ?3, relation_name = ?4 WHERE schema_name = ?1 AND relation_name = ?2",
                params![from.schema, from.name, to.schema, to.name],
            )?;
            if updated != 1 {
                return Err(SQLiteError::StorageBackend(format!(
                    "view `{}` disappeared during rename",
                    from.qualified_name()
                )));
            }
            Self::release_relation(&tx, from, RelationKind::View)?;
            tx.commit()?;
            Ok(true)
        })
    }

    pub fn drop_view(&self, relation: &RelationIdentity) -> Result<bool> {
        if let Some(removed) =
            self.drop_native_relation(super::native::RelationRecord::View, relation)?
        {
            return Ok(removed);
        }
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
        if let Some(views) = self.load_native_views()? {
            return Ok(views);
        }
        self.conn.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT schema_name, relation_name, role_owner, acl_json, column_acls_json, definition_json
                   FROM _views ORDER BY schema_name, relation_name",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, rusqlite::types::Value>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
            let mut views = Vec::new();
            for row in rows {
                let (schema, name, role_owner, acl_json, column_acls_json, definition_json) = row?;
                views.push(ViewRow {
                    relation: RelationIdentity::new(schema, name),
                    security: super::role_security::decode_relation((&role_owner).into(), acl_json.as_deref(), column_acls_json.as_deref())?,
                    definition_json,
                });
            }
            Ok(views)
        })
    }
}
