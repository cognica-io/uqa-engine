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

impl Engine {
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
        let Some((canonical, kind)) = self
            .try_resolve_relation_kind(relation_name)
            .map_err(|error| SQLError::Internal(format!("resolve relation: {error}")))?
        else {
            return Err(SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{relation_name}\" does not exist"),
            });
        };
        if kind != "table" {
            return Ok(Value::Null);
        }
        let table = self
            .try_table(&canonical)
            .map_err(|error| SQLError::Internal(format!("load table `{canonical}`: {error}")))?
            .ok_or_else(|| SQLError::Internal(format!("table `{canonical}` disappeared")))?;
        let column_object_id = table
            .columns
            .read()
            .iter()
            .find(|column| column.name == *column_name)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42703".into(),
                message: format!(
                    "column \"{column_name}\" of relation \"{}\" does not exist",
                    Self::resolved_relation_identity(&canonical)
                        .map(|relation| relation.name)
                        .unwrap_or(canonical.clone())
                ),
            })?
            .object_id
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "column `{canonical}`.`{column_name}` has no object identity"
                ))
            })?;
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
                    owner.table_object_id == table.object_id()
                        && owner.column_object_id == column_object_id
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
            for column in columns {
                let column_object_id = column.object_id.ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "column `{}`.`{}` has no object identity during sequence-owner migration",
                        table.relation.qualified_name(),
                        column.name
                    ))
                })?;
                valid_owners.insert((table.object_id, column_object_id));
                let Some(provenance) = column.auto_increment.as_ref() else {
                    continue;
                };
                let Some(named_owner) = provenance.owner.as_ref() else {
                    continue;
                };
                if !stored_owner_names_current(&table.relation, &column, named_owner) {
                    continue;
                }
                let Some(sequence) = provenance.sequence.as_deref() else {
                    continue;
                };
                let sequence = resolve_migrated_sequence_reference(sequence, &sequence_relations)?;
                let owner = SequenceOwner {
                    table_object_id: table.object_id,
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
        let (table_name, kind) = self
            .try_resolve_relation_kind(table)
            .map_err(|error| SQLError::Internal(format!("resolve OWNED BY relation: {error}")))?
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42P01".into(),
                message: format!("relation \"{table}\" does not exist"),
            })?;
        if kind != "table" {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: format!("sequence cannot be owned by relation \"{table_name}\""),
            });
        }
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
        let table_state = self
            .try_table(&table_name)
            .map_err(|error| SQLError::Internal(format!("load OWNED BY table: {error}")))?
            .ok_or_else(|| SQLError::Internal(format!("table `{table_name}` disappeared")))?;
        let columns = table_state.columns.read();
        let owner_column = columns
            .iter()
            .find(|candidate| candidate.name == *column)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42703".into(),
                message: format!(
                    "column \"{column}\" of relation \"{}\" does not exist",
                    table_relation.name
                ),
            })?;
        let column_object_id = owner_column.object_id.ok_or_else(|| {
            SQLError::Internal(format!(
                "column `{table_name}`.`{column}` has no object identity"
            ))
        })?;
        Ok(Some(SequenceOwner {
            table_object_id: table_state.object_id(),
            column_object_id,
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
        let table_object_id = table.object_id();
        let columns = table.columns.read().clone();
        for column in columns {
            let Some(provenance) = column.auto_increment.as_ref() else {
                continue;
            };
            let Some(named_owner) = provenance.owner.as_ref() else {
                continue;
            };
            if named_owner.table != table_name || named_owner.column != column.name {
                continue;
            }
            let Some(sequence) = provenance.sequence.as_deref() else {
                continue;
            };
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
            self.attach_sequence_owner_identity(
                sequence,
                SequenceOwner {
                    table_object_id,
                    column_object_id,
                    dependency,
                },
            )
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        }
        Ok(())
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
                    .filter(|(table, column)| table != &canonical || column != column_name)
                    .map(|(table, column)| {
                        format!("default or generated expression on {table}.{column}")
                    }),
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

    pub(crate) fn sequence_owner_target(&self, owner: SequenceOwner) -> Option<(String, String)> {
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
                    .map(|column| (table_name, column.name.clone()))
            })
    }
}
