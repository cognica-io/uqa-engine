//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence dependency classification and dependency-aware DROP execution.

use crate::{
    Engine, RelationIdentity, SQLError, SequenceOwnerDependency, StorageBackendError,
    StorageBackendResult,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SequenceSchemaDependent {
    Default {
        table: String,
        column: String,
        foreign: bool,
    },
    GeneratedColumn {
        table: String,
        column: String,
        foreign: bool,
    },
    CheckConstraint {
        table: String,
        constraint: String,
        foreign: bool,
    },
}

impl SequenceSchemaDependent {
    pub(crate) fn table(&self) -> &str {
        match self {
            Self::Default { table, .. }
            | Self::GeneratedColumn { table, .. }
            | Self::CheckConstraint { table, .. } => table,
        }
    }

    pub(crate) fn is_column(&self, table_name: &str, column_name: &str) -> bool {
        match self {
            Self::Default { table, column, .. } | Self::GeneratedColumn { table, column, .. } => {
                table == table_name && column == column_name
            }
            Self::CheckConstraint { .. } => false,
        }
    }

    pub(crate) fn object_label(&self) -> String {
        match self {
            Self::Default {
                table,
                column,
                foreign,
            } => format!(
                "default value for column {column} of {} {table}",
                relation_kind(*foreign)
            ),
            Self::GeneratedColumn {
                table,
                column,
                foreign,
            } => format!("column {column} of {} {table}", relation_kind(*foreign)),
            Self::CheckConstraint {
                table,
                constraint,
                foreign,
            } => format!(
                "constraint {constraint} on {} {table}",
                relation_kind(*foreign)
            ),
        }
    }

    fn restrict_label(&self) -> String {
        match self {
            Self::Default { table, column, .. } | Self::GeneratedColumn { table, column, .. } => {
                format!("{table}.{column}")
            }
            Self::CheckConstraint {
                table,
                constraint,
                foreign,
            } => format!(
                "constraint {constraint} on {} {table}",
                relation_kind(*foreign)
            ),
        }
    }
}

fn relation_kind(foreign: bool) -> &'static str {
    if foreign {
        "foreign table"
    } else {
        "table"
    }
}

struct SequenceDropDependents {
    schema: Vec<SequenceSchemaDependent>,
    views: Vec<String>,
    rules: Vec<(RelationIdentity, String)>,
}

impl SequenceDropDependents {
    fn ensure_empty_for_restrict(&self, name: &str) -> Result<(), SQLError> {
        if self.schema.is_empty() && self.views.is_empty() && self.rules.is_empty() {
            return Ok(());
        }
        let mut dependents = self
            .schema
            .iter()
            .map(SequenceSchemaDependent::restrict_label)
            .collect::<Vec<_>>();
        dependents.extend(self.views.iter().map(|view| format!("view {view}")));
        dependents.extend(
            self.rules
                .iter()
                .map(|(table, rule)| format!("rule {rule} on table {}", table.qualified_name())),
        );
        Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message: format!(
                "cannot drop sequence {name} because other objects depend on it: {}",
                dependents.join(", ")
            ),
        })
    }
}

impl Engine {
    pub fn drop_sequence(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| engine.drop_sequence_inner(name))
    }

    fn drop_sequence_inner(&self, name: &str) -> Result<bool, String> {
        let Some(name) = self
            .try_resolve_sequence_name(name)
            .map_err(|err| format!("load sequence catalog: {err}"))?
        else {
            return Ok(false);
        };
        self.drop_sequences_sql_inner(std::slice::from_ref(&name), false)
            .map_err(|error| error.to_string())?;
        Ok(true)
    }

    pub(crate) fn drop_sequences_sql_inner(
        &self,
        names: &[String],
        cascade: bool,
    ) -> Result<(), SQLError> {
        self.drop_sequences_sql_inner_with_owner(names, cascade, false)
    }

    fn sequence_drop_dependents(&self, name: &str) -> Result<SequenceDropDependents, SQLError> {
        let schema = self
            .sequence_schema_expression_dependents(name)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "inspect column dependencies for sequence `{name}`: {error}"
                ))
            })?;
        let views = self.views_depending_on_sequence(name).map_err(|error| {
            SQLError::Internal(format!(
                "inspect view dependencies for sequence `{name}`: {error}"
            ))
        })?;
        let rules = self
            .rules_depending_on_relations(&[name.to_string()])
            .map_err(|error| {
                SQLError::Internal(format!(
                    "inspect rule dependencies for sequence `{name}`: {error}"
                ))
            })?;
        Ok(SequenceDropDependents {
            schema,
            views,
            rules,
        })
    }

    fn drop_rules_for_sequence_cascade(
        &self,
        names: &[String],
        cascade_views: &[String],
    ) -> Result<(), SQLError> {
        self.drop_rules_depending_on_relations_inner(names)
            .map_err(|error| {
                SQLError::Internal(format!("drop rules depending on sequence: {error}"))
            })?;
        if !cascade_views.is_empty() {
            self.drop_rules_depending_on_relations_inner(cascade_views)
                .map_err(|error| {
                    SQLError::Internal(format!("drop rules depending on cascading views: {error}"))
                })?;
        }
        Ok(())
    }

    fn drop_sequences_sql_inner_with_owner(
        &self,
        names: &[String],
        cascade: bool,
        owner_initiated: bool,
    ) -> Result<(), SQLError> {
        let mut cascade_schema = Vec::new();
        let mut direct_views = Vec::new();
        for name in names {
            if !owner_initiated {
                let relation = Self::resolved_relation_identity(name).map_err(|error| {
                    SQLError::Internal(format!("resolve sequence `{name}`: {error}"))
                })?;
                self.ensure_sequence_owner(name, &relation)?;
                let owner = self
                    .durable
                    .sequences
                    .read()
                    .get(&relation)
                    .and_then(|state| state.owner)
                    .filter(|owner| owner.dependency == SequenceOwnerDependency::Internal);
                if let Some(owner) = owner {
                    let (table, column, foreign) =
                        self.sequence_owner_target(owner).ok_or_else(|| {
                            SQLError::Internal(format!(
                                "identity sequence `{name}` has a dangling owner dependency"
                            ))
                        })?;
                    let relation_kind = if foreign { "foreign table" } else { "table" };
                    return Err(SQLError::Routine {
                        sqlstate: "2BP01".into(),
                        message: format!(
                            "cannot drop sequence {name} because column {column} of {relation_kind} {table} requires it"
                        ),
                    });
                }
            }
            let dependents = self.sequence_drop_dependents(name)?;
            if !cascade {
                dependents.ensure_empty_for_restrict(name)?;
            }
            cascade_schema.extend(dependents.schema);
            direct_views.extend(dependents.views);
        }
        cascade_schema.sort();
        cascade_schema.dedup();
        let cascade_views = self.cascade_view_closure(direct_views)?;
        if cascade {
            self.drop_rules_for_sequence_cascade(names, &cascade_views)?;
        }
        if cascade && !cascade_views.is_empty() {
            self.drop_views_inner(&cascade_views, false)?;
        }
        for name in names {
            self.detach_sequence_column_dependencies(name, cascade)
                .map_err(|error| {
                    SQLError::Internal(format!(
                        "detach column dependencies for sequence `{name}`: {error}"
                    ))
                })?;
            if !self
                .remove_sequence_state_inner(name)
                .map_err(SQLError::Internal)?
            {
                return Err(SQLError::Internal(format!(
                    "resolved sequence `{name}` disappeared before DROP"
                )));
            }
        }
        if cascade {
            let mut dependents = cascade_schema
                .iter()
                .map(SequenceSchemaDependent::object_label)
                .collect::<Vec<_>>();
            dependents.extend(cascade_views.iter().map(|view| format!("view {view}")));
            match dependents.as_slice() {
                [] => {}
                [dependent] => {
                    self.push_sql_notice("NOTICE", &format!("drop cascades to {dependent}"));
                }
                _ => self.push_sql_notice(
                    "NOTICE",
                    &format!("drop cascades to {} other objects", dependents.len()),
                ),
            }
        }
        Ok(())
    }

    fn remove_sequence_state_inner(&self, name: &str) -> Result<bool, String> {
        let relation = Self::resolved_relation_identity(name)
            .map_err(|err| format!("resolve sequence `{name}`: {err}"))?;
        let object_id = self
            .durable
            .sequence_object_ids
            .read()
            .get(&relation)
            .copied()
            .ok_or_else(|| format!("Sequence `{name}` has no object identity"))?;
        let temporary = self
            .durable
            .sequence_persistence
            .read()
            .get(&relation)
            .is_some_and(|persistence| {
                *persistence == uqa_sql::ast::RelationPersistence::Temporary
            });
        let removed = if temporary {
            self.durable.sequences.read().contains_key(&relation)
        } else if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_sequence_row(name)
                .map_err(|err| format!("persist sequence catalog: {err}"))?
        } else {
            self.durable.sequences.read().contains_key(&relation)
        };
        if removed {
            self.durable.sequences.write().remove(&relation);
            self.durable.sequence_object_ids.write().remove(&relation);
            self.durable.sequence_persistence.write().remove(&relation);
            self.durable.sequence_security.write().remove(&relation);
            let mut session = self.session.state.write();
            session
                .sequence_currvals
                .retain(|_, current| current.object_id != object_id);
            if session
                .last_sequence
                .as_ref()
                .is_some_and(|last| last.object_id == object_id)
            {
                session.last_sequence = None;
            }
            drop(session);
            self.session
                .sequence_caches
                .lock()
                .retain(|_, cache| cache.object_id != object_id);
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub(crate) fn drop_owned_sequence(
        &self,
        name: &str,
        cascade: bool,
    ) -> StorageBackendResult<()> {
        let canonical = self.try_resolve_sequence_name(name)?.ok_or_else(|| {
            StorageBackendError::Other(format!("owned sequence `{name}` does not exist"))
        })?;
        self.drop_sequences_sql_inner_with_owner(std::slice::from_ref(&canonical), cascade, true)
            .map_err(|error| StorageBackendError::Other(error.to_string()))
    }
}
