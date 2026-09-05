//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable table-column dependencies for SQL sequences.

use super::{
    CatalogFacade, Engine, RelationIdentity, SQLError, SequenceOwner, SequenceOwnerDependency,
    StorageBackendError, StorageBackendResult, Value,
};

fn stored_owner_names_current(
    table: &RelationIdentity,
    column: &uqa_sql::ast::ColumnDef,
    owner: &uqa_sql::ast::AutoIncrementOwner,
) -> bool {
    let table_matches =
        RelationIdentity::parse_reference(&owner.table).is_ok_and(|(schema, name)| {
            schema.is_none_or(|schema| schema == table.schema) && name == table.name
        });
    table_matches && owner.column == column.name
}

fn resolve_migrated_sequence_reference(
    reference: &str,
    sequences: &[RelationIdentity],
) -> StorageBackendResult<RelationIdentity> {
    let (schema, name) =
        RelationIdentity::parse_reference(reference).map_err(StorageBackendError::Other)?;
    let candidates = sequences
        .iter()
        .filter(|candidate| {
            candidate.name == name
                && schema
                    .as_ref()
                    .is_none_or(|schema| candidate.schema == *schema)
        })
        .cloned()
        .collect::<Vec<_>>();
    match candidates.as_slice() {
        [target] => Ok(target.clone()),
        [] => Err(StorageBackendError::Other(format!(
            "implicit sequence owner references missing sequence `{reference}`"
        ))),
        _ => Err(StorageBackendError::Other(format!(
            "implicit sequence owner reference `{reference}` is ambiguous"
        ))),
    }
}

fn implicit_sequence_data_type(
    column: &uqa_sql::ast::ColumnDef,
) -> StorageBackendResult<uqa_sql::ast::SequenceDataType> {
    match &column.ty {
        uqa_sql::ast::ColumnType::SmallInteger => Ok(uqa_sql::ast::SequenceDataType::SmallInt),
        uqa_sql::ast::ColumnType::Integer => Ok(uqa_sql::ast::SequenceDataType::Integer),
        uqa_sql::ast::ColumnType::BigInteger => Ok(uqa_sql::ast::SequenceDataType::BigInt),
        _ => Err(StorageBackendError::Other(format!(
            "implicit sequence column `{}` has non-integer type",
            column.name
        ))),
    }
}

const POSTGRES_IDENTIFIER_MAX_BYTES: usize = 63;

fn clip_identifier_component(value: &str, byte_length: usize) -> &str {
    let mut end = byte_length.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn implicit_sequence_local_name(
    table: &str,
    column: &str,
    collision_pass: usize,
) -> StorageBackendResult<String> {
    let label = if collision_pass == 0 {
        "seq".to_string()
    } else {
        format!("seq{collision_pass}")
    };
    let overhead = label.len() + 2;
    let available = POSTGRES_IDENTIFIER_MAX_BYTES
        .checked_sub(overhead)
        .filter(|available| *available > 0)
        .ok_or_else(|| {
            StorageBackendError::Other(format!(
                "cannot generate an implicit sequence name with label `{label}`"
            ))
        })?;
    let mut table_bytes = table.len();
    let mut column_bytes = column.len();
    while table_bytes + column_bytes > available {
        if table_bytes > column_bytes {
            table_bytes -= 1;
        } else {
            column_bytes -= 1;
        }
    }
    let table = clip_identifier_component(table, table_bytes);
    let column = clip_identifier_component(column, column_bytes);
    Ok(format!("{table}_{column}_{label}"))
}

fn choose_implicit_sequence_name(
    table: &RelationIdentity,
    column: &str,
    mut collides: impl FnMut(&RelationIdentity) -> StorageBackendResult<bool>,
) -> StorageBackendResult<String> {
    for collision_pass in 0.. {
        let candidate = RelationIdentity::new(
            table.schema.clone(),
            implicit_sequence_local_name(&table.name, column, collision_pass)?,
        );
        if !collides(&candidate)? {
            return Ok(candidate.qualified_name());
        }
    }
    unreachable!("the collision pass is unbounded")
}

fn persisted_relation_names(
    catalog: &dyn CatalogFacade,
) -> StorageBackendResult<std::collections::BTreeSet<RelationIdentity>> {
    let mut relations = std::collections::BTreeSet::new();
    relations.extend(catalog.load_tables()?.into_iter().map(|row| row.relation));
    relations.extend(
        catalog
            .load_sequence_rows()?
            .into_iter()
            .map(|row| row.relation),
    );
    relations.extend(catalog.load_views()?.into_iter().map(|row| row.relation));
    relations.extend(
        catalog
            .load_foreign_tables()?
            .into_iter()
            .map(|row| row.relation),
    );
    relations.extend(
        catalog
            .load_catalog_indexes()?
            .into_iter()
            .map(|row| row.relation),
    );
    Ok(relations)
}

fn apply_implicit_sequence_metadata(
    table_name: &str,
    column: &mut uqa_sql::ast::ColumnDef,
    sequence: String,
) -> StorageBackendResult<()> {
    let auto_increment = column.auto_increment.as_mut().ok_or_else(|| {
        StorageBackendError::Other(format!(
            "implicit sequence column `{table_name}`.`{}` lost its generation metadata",
            column.name
        ))
    })?;
    auto_increment.sequence = Some(sequence.clone());
    auto_increment.owner = Some(uqa_sql::ast::AutoIncrementOwner {
        table: table_name.to_string(),
        column: column.name.clone(),
    });
    if auto_increment.kind == uqa_sql::ast::AutoIncrementKind::Serial {
        column.default = Some(uqa_sql::ast::Expr::Func {
            name: "nextval".into(),
            binding: None,
            args: vec![uqa_sql::ast::Expr::Literal(Value::Str(sequence))],
            distinct: false,
            order_by: Vec::new(),
            filter: None,
        });
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct SequenceOwnerColumnIdentity {
    table_object_id: [u8; 16],
    column_object_id: [u8; 16],
}

fn collect_migrated_sequence_owner(
    relation: &RelationIdentity,
    table_object_id: [u8; 16],
    column: &uqa_sql::ast::ColumnDef,
    sequence_relations: &[RelationIdentity],
    valid_owners: &mut std::collections::BTreeSet<([u8; 16], [u8; 16])>,
    inferred: &mut std::collections::BTreeMap<RelationIdentity, SequenceOwner>,
) -> StorageBackendResult<()> {
    let relation_name = relation.qualified_name();
    let column_object_id = column.object_id.ok_or_else(|| {
        StorageBackendError::Other(format!(
            "column `{relation_name}`.`{}` has no object identity during sequence-owner migration",
            column.name
        ))
    })?;
    valid_owners.insert((table_object_id, column_object_id));
    let Some(provenance) = column.auto_increment.as_ref() else {
        return Ok(());
    };
    let Some(named_owner) = provenance.owner.as_ref() else {
        return Ok(());
    };
    if !stored_owner_names_current(relation, column, named_owner) {
        return Ok(());
    }
    let Some(sequence) = provenance.sequence.as_deref() else {
        return Ok(());
    };
    let sequence = resolve_migrated_sequence_reference(sequence, sequence_relations)?;
    let owner = SequenceOwner {
        table_object_id,
        column_object_id,
        dependency: if provenance.is_identity() {
            SequenceOwnerDependency::Internal
        } else {
            SequenceOwnerDependency::Automatic
        },
    };
    if inferred
        .insert(sequence.clone(), owner)
        .is_some_and(|old| old != owner)
    {
        return Err(StorageBackendError::Other(format!(
            "sequence `{}` has conflicting implicit owners",
            sequence.qualified_name()
        )));
    }
    Ok(())
}

impl Engine {
    fn sequence_owner_column_identity(
        &self,
        canonical: &str,
        relation_kind: &str,
        column_name: &str,
    ) -> Result<Option<SequenceOwnerColumnIdentity>, SQLError> {
        let relation = Self::resolved_relation_identity(canonical).map_err(|error| {
            SQLError::Internal(format!("resolve sequence owner `{canonical}`: {error}"))
        })?;
        let missing_column = || SQLError::Routine {
            sqlstate: "42703".into(),
            message: format!(
                "column \"{column_name}\" of relation \"{}\" does not exist",
                relation.name
            ),
        };
        match relation_kind {
            "table" => {
                let table = self
                    .try_table(canonical)
                    .map_err(|error| {
                        SQLError::Internal(format!("load table `{canonical}`: {error}"))
                    })?
                    .ok_or_else(|| {
                        SQLError::Internal(format!("table `{canonical}` disappeared"))
                    })?;
                let column_object_id = table
                    .columns
                    .read()
                    .iter()
                    .find(|column| column.name == column_name)
                    .ok_or_else(missing_column)?
                    .object_id
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "column `{canonical}`.`{column_name}` has no object identity"
                        ))
                    })?;
                Ok(Some(SequenceOwnerColumnIdentity {
                    table_object_id: table.object_id(),
                    column_object_id,
                }))
            }
            "foreign table" => {
                let table = self
                    .durable
                    .foreign_tables
                    .read()
                    .get(&relation)
                    .cloned()
                    .ok_or_else(|| {
                        SQLError::Internal(format!("foreign table `{canonical}` disappeared"))
                    })?;
                let column_object_id = table
                    .columns
                    .iter()
                    .find(|column| column.name == column_name)
                    .ok_or_else(missing_column)?
                    .object_id
                    .ok_or_else(|| {
                        SQLError::Internal(format!(
                            "column `{canonical}`.`{column_name}` has no object identity"
                        ))
                    })?;
                Ok(Some(SequenceOwnerColumnIdentity {
                    table_object_id: table.object_id,
                    column_object_id,
                }))
            }
            _ => Ok(None),
        }
    }

    pub(crate) fn materialize_implicit_sequences(
        &self,
        statement: &str,
        table_name: &str,
        columns: &mut [uqa_sql::ast::ColumnDef],
        persistence: uqa_sql::ast::RelationPersistence,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(table_name).map_err(|error| {
            SQLError::Internal(format!("resolve {statement} relation: {error}"))
        })?;
        let mut sequences = Vec::new();
        for (column_index, column) in columns.iter().enumerate() {
            let Some(auto_increment) = column.auto_increment.as_ref() else {
                continue;
            };
            if auto_increment.kind == uqa_sql::ast::AutoIncrementKind::Legacy
                || auto_increment.sequence.is_some()
            {
                continue;
            }
            let data_type = implicit_sequence_data_type(column)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
            let sequence = choose_implicit_sequence_name(&relation, &column.name, |candidate| {
                self.relation_kind_at(&candidate.qualified_name())
                    .map(|kind| kind.is_some())
            })
            .map_err(|error| {
                SQLError::Internal(format!(
                    "choose implicit sequence for `{table_name}`.`{}`: {error}",
                    column.name
                ))
            })?;
            sequences.push((column_index, data_type, sequence));
        }
        for (column_index, data_type, sequence) in sequences {
            self.create_implicit_sequence_with_persistence(
                &sequence,
                1,
                1,
                data_type,
                persistence,
            )?;
            apply_implicit_sequence_metadata(table_name, &mut columns[column_index], sequence)
                .map_err(|error| SQLError::Internal(error.to_string()))?;
        }
        Ok(())
    }

    pub(crate) fn materialize_persisted_foreign_implicit_sequences(
        catalog: &dyn CatalogFacade,
        relation: &RelationIdentity,
        role_owner: &str,
        table_object_id: [u8; 16],
        columns: &mut [uqa_sql::ast::ColumnDef],
    ) -> StorageBackendResult<bool> {
        let table_name = relation.qualified_name();
        let occupied_relations = persisted_relation_names(catalog)?;
        let mut sequences = Vec::new();
        for (column_index, column) in columns.iter().enumerate() {
            let Some(auto_increment) = column.auto_increment.as_ref() else {
                continue;
            };
            if auto_increment.kind == uqa_sql::ast::AutoIncrementKind::Legacy
                || auto_increment.sequence.is_some()
            {
                continue;
            }
            let data_type = implicit_sequence_data_type(column)?;
            let sequence = choose_implicit_sequence_name(relation, &column.name, |candidate| {
                Ok(occupied_relations.contains(candidate))
            })?;
            sequences.push((column_index, data_type, sequence));
        }
        let mut changed = false;
        for (column_index, data_type, sequence) in sequences {
            let column = &mut columns[column_index];
            let auto_increment = column.auto_increment.as_ref().ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "implicit sequence column `{table_name}`.`{}` lost its generation metadata",
                    column.name
                ))
            })?;
            let column_object_id = column.object_id.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "column `{table_name}`.`{}` has no object identity during implicit sequence migration",
                    column.name
                ))
            })?;
            let mut state = Self::default_sequence_state(1, 1, data_type);
            state.owner = Some(SequenceOwner {
                table_object_id,
                column_object_id,
                dependency: if auto_increment.is_identity() {
                    SequenceOwnerDependency::Internal
                } else {
                    SequenceOwnerDependency::Automatic
                },
            });
            let object_id = crate::new_sequence_object_id()?;
            state.definition_generation = object_id;
            let security = crate::engine_state::SequenceSecurity {
                role_owner: role_owner.to_string(),
                acl: None,
            };
            if !catalog.create_sequence_row(&Self::sequence_row(
                &sequence,
                object_id,
                state,
                uqa_sql::ast::RelationPersistence::Permanent,
                &security,
            )?)? {
                return Err(StorageBackendError::Other(format!(
                    "cannot migrate implicit sequence `{sequence}` because that relation name already exists"
                )));
            }
            apply_implicit_sequence_metadata(&table_name, column, sequence)?;
            changed = true;
        }
        Ok(changed)
    }

    pub(crate) fn pg_get_serial_sequence_value(
        &self,
        arguments: &[Value],
    ) -> Result<Value, SQLError> {
        if arguments.len() != 2 {
            return Err(SQLError::BadArity {
                name: "pg_get_serial_sequence".into(),
                expected: "2".into(),
                actual: arguments.len(),
            });
        }
        if arguments
            .iter()
            .any(|argument| matches!(argument, Value::Null))
        {
            return Ok(Value::Null);
        }
        let relation_name = match &arguments[0] {
            Value::Str(value) | Value::FixedChar(value) => value,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "pg_get_serial_sequence table name must be text, got {other:?}"
                )))
            }
        };
        let column_name = match &arguments[1] {
            Value::Str(value) | Value::FixedChar(value) => value,
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "pg_get_serial_sequence column name must be text, got {other:?}"
                )))
            }
        };
        let Some((canonical, kind)) = self.try_resolve_visible_relation_kind(relation_name)? else {
            return Err(SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{relation_name}\" does not exist"),
            });
        };
        let Some(owner_column) =
            self.sequence_owner_column_identity(&canonical, kind, column_name)?
        else {
            return Ok(Value::Null);
        };
        self.refresh_sequences_from_catalog().map_err(|error| {
            SQLError::Internal(format!("load sequence ownership catalog: {error}"))
        })?;
        let sequence = self
            .durable
            .sequences
            .read()
            .iter()
            .find(|(_, state)| {
                state.owner.is_some_and(|owner| {
                    owner.table_object_id == owner_column.table_object_id
                        && owner.column_object_id == owner_column.column_object_id
                })
            })
            .map(|(relation, _)| relation.qualified_name());
        Ok(sequence.map_or(Value::Null, Value::Str))
    }

    pub(crate) fn migrate_implicit_sequence_owners(
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let tables = catalog.load_tables()?;
        let rows = catalog.load_sequence_rows()?;
        let sequence_relations = rows
            .iter()
            .map(|row| row.relation.clone())
            .collect::<Vec<_>>();
        let mut valid_owners = std::collections::BTreeSet::new();
        let mut inferred = std::collections::BTreeMap::new();
        for table in tables {
            let columns: Vec<uqa_sql::ast::ColumnDef> = if table.columns_json.is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&table.columns_json)?
            };
            for column in &columns {
                collect_migrated_sequence_owner(
                    &table.relation,
                    table.object_id,
                    column,
                    &sequence_relations,
                    &mut valid_owners,
                    &mut inferred,
                )?;
            }
        }
        for row in catalog.load_foreign_tables()? {
            let relation_name = row.relation.qualified_name();
            let options = serde_json::from_str(&row.options_json)?;
            let (table, _) = crate::engine_fdw::StoredForeignTable::from_catalog(
                relation_name.clone(),
                row.server_name,
                options,
                &row.columns_json,
            )?;
            if table.object_id == [0; 16] {
                return Err(StorageBackendError::Other(format!(
                    "foreign table `{relation_name}` has no object identity during sequence-owner migration"
                )));
            }
            for column in &table.columns {
                collect_migrated_sequence_owner(
                    &row.relation,
                    table.object_id,
                    column,
                    &sequence_relations,
                    &mut valid_owners,
                    &mut inferred,
                )?;
            }
        }
        for mut row in rows {
            if let Some(owner) = row.owner {
                if !valid_owners.contains(&(owner.table_object_id, owner.column_object_id)) {
                    return Err(StorageBackendError::Other(format!(
                        "sequence `{}` has a dangling owner dependency",
                        row.relation.qualified_name()
                    )));
                }
                continue;
            }
            let Some(owner) = inferred.get(&row.relation).copied() else {
                continue;
            };
            row.owner = Some(owner);
            if !catalog.replace_sequence_row(&row)? {
                return Err(StorageBackendError::Other(format!(
                    "sequence `{}` disappeared during owner migration",
                    row.relation.qualified_name()
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn resolve_sequence_ownership(
        &self,
        sequence_name: &str,
        ownership: &uqa_sql::ast::SequenceOwnership,
    ) -> Result<Option<SequenceOwner>, SQLError> {
        let uqa_sql::ast::SequenceOwnership::Column { table, column } = ownership else {
            return Ok(None);
        };
        let (table_name, kind) =
            self.try_resolve_visible_relation_kind(table)?
                .ok_or_else(|| SQLError::Routine {
                    sqlstate: "42P01".into(),
                    message: format!("relation \"{table}\" does not exist"),
                })?;
        let Some(owner_column) = self.sequence_owner_column_identity(&table_name, kind, column)?
        else {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("sequence cannot be owned by relation \"{table_name}\""),
            });
        };
        let sequence_relation =
            Self::resolved_relation_identity(sequence_name).map_err(|error| {
                SQLError::Internal(format!(
                    "resolve sequence `{sequence_name}` ownership: {error}"
                ))
            })?;
        let table_relation = Self::resolved_relation_identity(&table_name).map_err(|error| {
            SQLError::Internal(format!("resolve table `{table_name}` ownership: {error}"))
        })?;
        if sequence_relation.schema != table_relation.schema {
            return Err(SQLError::Routine {
                sqlstate: "55000".into(),
                message: "sequence must be in same schema as table it is linked to".into(),
            });
        }
        Ok(Some(SequenceOwner {
            table_object_id: owner_column.table_object_id,
            column_object_id: owner_column.column_object_id,
            dependency: SequenceOwnerDependency::Automatic,
        }))
    }

    pub(crate) fn attach_implicit_sequence_owners(
        &self,
        table_name: &str,
    ) -> StorageBackendResult<()> {
        let table = self.try_table(table_name)?.ok_or_else(|| {
            StorageBackendError::Other(format!("table `{table_name}` disappeared"))
        })?;
        let columns = table.columns.read().clone();
        self.attach_implicit_sequence_owners_for_columns(table_name, table.object_id(), &columns)
    }

    pub(crate) fn attach_implicit_sequence_owners_for_columns(
        &self,
        table_name: &str,
        table_object_id: [u8; 16],
        columns: &[uqa_sql::ast::ColumnDef],
    ) -> StorageBackendResult<()> {
        for (sequence, owner) in
            self.implicit_sequence_owner_bindings(table_name, table_object_id, columns)?
        {
            self.attach_sequence_owner_identity(&sequence, owner)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        Ok(())
    }

    pub(crate) fn validate_implicit_sequence_owners_for_columns(
        &self,
        table_name: &str,
        table_object_id: [u8; 16],
        columns: &[uqa_sql::ast::ColumnDef],
    ) -> StorageBackendResult<()> {
        for (sequence, expected) in
            self.implicit_sequence_owner_bindings(table_name, table_object_id, columns)?
        {
            let relation = RelationIdentity::from_legacy_name(&sequence)
                .map_err(StorageBackendError::Other)?;
            let actual = self
                .durable
                .sequences
                .read()
                .get(&relation)
                .and_then(|state| state.owner);
            if actual != Some(expected) {
                return Err(StorageBackendError::Other(format!(
                    "implicit sequence `{sequence}` for `{table_name}` has stale owner metadata that requires an initial-open migration"
                )));
            }
        }
        Ok(())
    }

    fn implicit_sequence_owner_bindings(
        &self,
        table_name: &str,
        table_object_id: [u8; 16],
        columns: &[uqa_sql::ast::ColumnDef],
    ) -> StorageBackendResult<Vec<(String, SequenceOwner)>> {
        let relation =
            RelationIdentity::from_legacy_name(table_name).map_err(StorageBackendError::Other)?;
        let mut bindings = Vec::new();
        for column in columns {
            let Some(provenance) = column.auto_increment.as_ref() else {
                continue;
            };
            let Some(named_owner) = provenance.owner.as_ref() else {
                continue;
            };
            if !stored_owner_names_current(&relation, column, named_owner) {
                continue;
            }
            let Some(sequence) = provenance.sequence.as_deref() else {
                continue;
            };
            let sequence = self.resolve_stored_sequence_reference_from_loaded_registry(sequence)?;
            let column_object_id = column.object_id.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "column `{table_name}`.`{}` has no object identity",
                    column.name
                ))
            })?;
            let dependency = if provenance.is_identity() {
                SequenceOwnerDependency::Internal
            } else {
                SequenceOwnerDependency::Automatic
            };
            bindings.push((
                sequence,
                SequenceOwner {
                    table_object_id,
                    column_object_id,
                    dependency,
                },
            ));
        }
        Ok(bindings)
    }

    fn attach_sequence_owner_identity(
        &self,
        name: &str,
        owner: SequenceOwner,
    ) -> Result<(), SQLError> {
        let canonical = self
            .try_resolve_sequence_name(name)
            .map_err(|error| SQLError::Internal(format!("resolve sequence `{name}`: {error}")))?
            .ok_or_else(|| SQLError::Internal(format!("sequence `{name}` disappeared")))?;
        let relation = Self::resolved_relation_identity(&canonical).map_err(|error| {
            SQLError::Internal(format!("resolve sequence `{canonical}`: {error}"))
        })?;
        let persistence = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .copied()
            .unwrap_or_default();
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| {
                SQLError::Internal(format!("sequence `{canonical}` has no object identity"))
            })?;
        let mut state = self
            .durable
            .sequences
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| SQLError::Internal(format!("sequence `{canonical}` disappeared")))?;
        if state.owner == Some(owner) {
            return Ok(());
        }
        if state.owner.is_some() {
            return Err(SQLError::Internal(format!(
                "implicit sequence `{canonical}` already has another owner"
            )));
        }
        state.owner = Some(owner);
        state.definition_generation =
            crate::new_sequence_definition_generation().map_err(|error| {
                SQLError::Internal(format!(
                    "allocate sequence `{canonical}` definition generation: {error}"
                ))
            })?;
        self.persist_sequence_state_replacement(
            &canonical,
            &relation,
            object_id,
            persistence,
            state,
            true,
        )
    }

    pub(crate) fn sequence_names_owned_by_tables(
        &self,
        table_object_ids: &std::collections::BTreeSet<[u8; 16]>,
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        self.refresh_sequences_from_catalog()?;
        Ok(self
            .durable
            .sequences
            .read()
            .iter()
            .filter(|(_, state)| {
                state
                    .owner
                    .is_some_and(|owner| table_object_ids.contains(&owner.table_object_id))
            })
            .map(|(relation, _)| relation.qualified_name())
            .collect())
    }

    pub(crate) fn foreign_table_owned_sequence_names(
        &self,
        table_names: &[String],
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        let mut table_object_ids = std::collections::BTreeSet::new();
        let tables = self.durable.foreign_tables.read();
        for table_name in table_names {
            let relation = RelationIdentity::from_legacy_name(table_name)
                .map_err(StorageBackendError::Other)?;
            let table = tables.get(&relation).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "foreign table `{table_name}` disappeared while resolving owned sequences"
                ))
            })?;
            table_object_ids.insert(table.object_id);
        }
        drop(tables);
        self.sequence_names_owned_by_tables(&table_object_ids)
    }

    pub(crate) fn sequence_external_dependents_for_owner_drop(
        &self,
        sequence: &str,
        owner_drop_targets: &std::collections::BTreeSet<String>,
    ) -> StorageBackendResult<Vec<String>> {
        let mut dependents = self
            .sequence_schema_expression_dependents(sequence)?
            .into_iter()
            .filter(|dependency| !owner_drop_targets.contains(dependency.table()))
            .map(|dependency| dependency.object_label())
            .collect::<Vec<_>>();
        dependents.extend(
            self.views_depending_on_sequence(sequence)?
                .into_iter()
                .map(|view| format!("view {view}")),
        );
        dependents.extend(
            self.rules_depending_on_relations(&[sequence.to_string()])?
                .into_iter()
                .map(|(table, rule)| format!("rule {rule} on table {}", table.qualified_name())),
        );
        dependents.sort_unstable();
        dependents.dedup();
        Ok(dependents)
    }

    pub(crate) fn sequence_names_owned_by_column(
        &self,
        table_object_id: [u8; 16],
        column_object_id: [u8; 16],
    ) -> StorageBackendResult<std::collections::BTreeSet<String>> {
        self.refresh_sequences_from_catalog()?;
        Ok(self
            .durable
            .sequences
            .read()
            .iter()
            .filter(|(_, state)| {
                state.owner.is_some_and(|owner| {
                    owner.table_object_id == table_object_id
                        && owner.column_object_id == column_object_id
                })
            })
            .map(|(relation, _)| relation.qualified_name())
            .collect())
    }

    pub(crate) fn owned_sequence_dependents_for_column(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<Vec<String>> {
        let canonical = self.try_resolve_table_name(table_name)?.ok_or_else(|| {
            StorageBackendError::Other(format!("table `{table_name}` does not exist"))
        })?;
        let table = self.try_table(&canonical)?.ok_or_else(|| {
            StorageBackendError::Other(format!("table `{canonical}` disappeared"))
        })?;
        let column_object_id = table
            .columns
            .read()
            .iter()
            .find(|column| column.name == column_name)
            .and_then(|column| column.object_id)
            .ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "column `{canonical}`.`{column_name}` has no object identity"
                ))
            })?;
        let mut dependents = Vec::new();
        for sequence in self.sequence_names_owned_by_column(table.object_id(), column_object_id)? {
            dependents.extend(
                self.sequence_schema_expression_dependents(&sequence)?
                    .into_iter()
                    .filter(|dependency| !dependency.is_column(&canonical, column_name))
                    .map(|dependency| dependency.object_label()),
            );
            dependents.extend(
                self.views_depending_on_sequence(&sequence)?
                    .into_iter()
                    .map(|view| format!("view {view}")),
            );
        }
        dependents.sort_unstable();
        dependents.dedup();
        Ok(dependents)
    }

    pub(crate) fn sequence_owner_target(
        &self,
        owner: SequenceOwner,
    ) -> Option<(String, String, bool)> {
        self.table_entries()
            .into_iter()
            .find_map(|(table_name, table)| {
                if table.object_id() != owner.table_object_id {
                    return None;
                }
                table
                    .columns
                    .read()
                    .iter()
                    .find(|column| column.object_id == Some(owner.column_object_id))
                    .map(|column| (table_name, column.name.clone(), false))
            })
            .or_else(|| {
                self.durable
                    .foreign_tables
                    .read()
                    .iter()
                    .find_map(|(relation, table)| {
                        if table.object_id != owner.table_object_id {
                            return None;
                        }
                        table
                            .columns
                            .iter()
                            .find(|column| column.object_id == Some(owner.column_object_id))
                            .map(|column| (relation.qualified_name(), column.name.clone(), true))
                    })
            })
    }
}
