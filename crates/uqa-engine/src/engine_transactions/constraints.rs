//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deferred constraint modes and transaction-boundary validation.

use super::{BTreeSet, ConstraintModeState, Engine, SQLError, TransactionFrame};
use crate::{ConstraintIdentity, RelationIdentity};
use uqa_sql::ast::{ForeignKey, SetConstraintName};

pub(crate) fn constraint_identities_match(
    left: &ConstraintIdentity,
    right: &ConstraintIdentity,
) -> bool {
    match (left.object_id, right.object_id) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

fn find_live_constraint_identity<'a>(
    live: &'a [ConstraintIdentity],
    live_relations: &BTreeSet<RelationIdentity>,
    identity: &ConstraintIdentity,
) -> Option<&'a ConstraintIdentity> {
    live.iter()
        .find(|current| *current == identity)
        .or_else(|| {
            if live_relations.contains(&identity.relation) {
                return None;
            }
            live.iter()
                .find(|current| constraint_identities_match(identity, current))
        })
}

fn constraint_is_deferred(
    modes: &ConstraintModeState,
    identity: &ConstraintIdentity,
    initially_deferred: bool,
) -> bool {
    modes
        .named
        .get(identity)
        .copied()
        .or_else(|| {
            modes.named.iter().find_map(|(candidate, deferred)| {
                constraint_identities_match(candidate, identity).then_some(*deferred)
            })
        })
        .or(modes.all)
        .unwrap_or(initially_deferred)
}

pub(crate) fn foreign_key_identity(
    table: &str,
    foreign_key: &ForeignKey,
) -> Result<ConstraintIdentity, SQLError> {
    let relation = RelationIdentity::from_legacy_name(table).map_err(|error| {
        SQLError::Internal(format!(
            "decode foreign-key relation identity '{table}': {error}"
        ))
    })?;
    let name = foreign_key.name.clone().ok_or_else(|| {
        SQLError::Internal(format!(
            "foreign key on '{table}' has no materialized constraint name"
        ))
    })?;
    let object_id = foreign_key.object_id.ok_or_else(|| {
        SQLError::Internal(format!(
            "foreign key '{name}' on '{table}' has no materialized object identity"
        ))
    })?;
    Ok(ConstraintIdentity {
        relation,
        name,
        object_id: Some(object_id),
    })
}

fn rendered_constraint_name(name: &SetConstraintName) -> String {
    [
        name.catalog.as_deref(),
        name.schema.as_deref(),
        Some(&name.name),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(".")
}

impl Engine {
    fn constraint_namespace_exists(&self, schema: &str) -> Result<bool, SQLError> {
        if schema == self.temporary_schema_name() {
            return Ok(self.temporary_namespace_allocated());
        }
        self.has_namespace(schema)
            .map_err(|error| SQLError::Internal(format!("resolve constraint schema: {error}")))
    }

    fn constraint_search_path(&self) -> Result<Vec<String>, SQLError> {
        let configured = self.search_path();
        let temporary = self.temporary_schema_name();
        let has_temporary = self.temporary_namespace_allocated();
        let temporary_is_explicit = configured
            .iter()
            .any(|schema| schema == "pg_temp" || schema == &temporary);
        let mut effective = Vec::new();
        if has_temporary && !temporary_is_explicit {
            effective.push(temporary.clone());
        }
        if !configured.iter().any(|schema| schema == "pg_catalog") {
            effective.push("pg_catalog".to_string());
        }
        for configured_schema in configured {
            let schema = match configured_schema.as_str() {
                "pg_temp" if has_temporary => temporary.clone(),
                "pg_temp" => continue,
                "$user" => self.current_user_name(),
                _ => configured_schema,
            };
            if self.constraint_namespace_exists(&schema)? && !effective.contains(&schema) {
                effective.push(schema);
            }
        }
        Ok(effective)
    }

    fn resolve_constraint_targets(
        &self,
        requested: &[SetConstraintName],
        deferred: bool,
    ) -> Result<BTreeSet<ConstraintIdentity>, SQLError> {
        let constraints = crate::sql::runtime_constraints(self)?;
        if requested.is_empty() {
            return Ok(constraints
                .iter()
                .filter(|constraint| constraint.deferrable)
                .map(|constraint| constraint.identity.clone())
                .collect());
        }

        let search_path = self.constraint_search_path()?;
        let mut targets = BTreeSet::new();
        for requested_name in requested {
            if requested_name
                .catalog
                .as_deref()
                .is_some_and(|catalog| catalog != "uqa")
            {
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!(
                        "cross-database references are not implemented: \"{}\"",
                        rendered_constraint_name(requested_name)
                    ),
                });
            }
            let explicit_schema = if let Some(schema) = requested_name.schema.as_deref() {
                let resolved = if schema == "pg_temp" {
                    self.temporary_schema_name()
                } else {
                    schema.to_string()
                };
                if !self.constraint_namespace_exists(&resolved)? {
                    return Err(SQLError::Routine {
                        sqlstate: "3F000".into(),
                        message: format!("schema \"{schema}\" does not exist"),
                    });
                }
                Some(resolved)
            } else {
                None
            };
            let matched_schema = explicit_schema.or_else(|| {
                search_path.iter().find_map(|schema| {
                    constraints
                        .iter()
                        .any(|constraint| {
                            constraint.identity.relation.schema == *schema
                                && constraint.identity.name == requested_name.name
                        })
                        .then(|| schema.clone())
                })
            });
            let matches = matched_schema.map_or_else(Vec::new, |schema| {
                constraints
                    .iter()
                    .filter(|constraint| {
                        constraint.identity.relation.schema == schema
                            && constraint.identity.name == requested_name.name
                    })
                    .collect::<Vec<_>>()
            });
            if matches.is_empty() {
                return Err(SQLError::Routine {
                    sqlstate: "42704".into(),
                    message: format!("constraint \"{}\" does not exist", requested_name.name),
                });
            }
            if deferred && matches.iter().any(|constraint| !constraint.deferrable) {
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("constraint \"{}\" is not deferrable", requested_name.name),
                });
            }
            targets.extend(
                matches
                    .into_iter()
                    .filter(|constraint| constraint.deferrable)
                    .map(|constraint| constraint.identity.clone()),
            );
        }
        Ok(targets)
    }

    pub(crate) fn set_constraints(
        &self,
        requested: &[SetConstraintName],
        deferred: bool,
        nested_statement: bool,
    ) -> Result<(), SQLError> {
        let transaction_block = self.in_transaction_block();
        let transaction_active =
            transaction_block || (nested_statement && self.transaction_depth() != 0);
        if !transaction_active {
            self.push_sql_notice(
                "WARNING",
                "SET CONSTRAINTS can only be used in transaction blocks",
            );
        }

        let mut targets = self.resolve_constraint_targets(requested, deferred)?;
        if !transaction_active {
            return Ok(());
        }

        let pending_checks = {
            let stack = self.session.transactions.lock();
            let frame = stack.last().ok_or_else(|| {
                SQLError::Internal("SET CONSTRAINTS lost its transaction frame".into())
            })?;
            frame.deferred_foreign_key_checks.clone()
        };
        if !deferred && requested.is_empty() {
            targets.extend(pending_checks.iter().map(|check| check.constraint.clone()));
        }
        if !deferred && !pending_checks.is_empty() {
            crate::sql::dml::validate_deferred_foreign_key_checks(
                self,
                &pending_checks,
                Some(&targets),
            )?;
        }

        let mut stack = self.session.transactions.lock();
        let frame = stack.last_mut().ok_or_else(|| {
            SQLError::Internal("SET CONSTRAINTS lost its transaction frame".into())
        })?;
        if !deferred {
            frame.deferred_foreign_key_checks.retain(|check| {
                !targets
                    .iter()
                    .any(|target| constraint_identities_match(target, &check.constraint))
            });
        }
        if requested.is_empty() {
            frame.constraint_modes.named.clear();
            frame.constraint_modes.all = Some(deferred);
        } else {
            for target in targets {
                frame
                    .constraint_modes
                    .named
                    .retain(|identity, _| !constraint_identities_match(identity, &target));
                frame.constraint_modes.named.insert(target, deferred);
            }
        }
        Ok(())
    }

    pub(crate) fn foreign_key_is_deferred(
        &self,
        table: &str,
        foreign_key: &ForeignKey,
    ) -> Result<bool, SQLError> {
        if !foreign_key.deferrable {
            return Ok(false);
        }
        let identity = self.foreign_key_constraint_identity(table, foreign_key)?;
        let stack = self.session.transactions.lock();
        Ok(stack
            .last()
            .map_or(foreign_key.initially_deferred, |frame| {
                constraint_is_deferred(
                    &frame.constraint_modes,
                    &identity,
                    foreign_key.initially_deferred,
                )
            }))
    }

    pub(crate) fn foreign_key_constraint_identity(
        &self,
        table: &str,
        foreign_key: &ForeignKey,
    ) -> Result<ConstraintIdentity, SQLError> {
        let canonical_table = self
            .try_resolve_table_name(table)
            .map_err(|error| {
                SQLError::Internal(format!(
                    "resolve foreign-key relation '{table}' for constraint mode: {error}"
                ))
            })?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        foreign_key_identity(&canonical_table, foreign_key)
    }

    pub(crate) fn rename_constraint_transaction_relation(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) {
        let mut stack = self.session.transactions.lock();
        let Some(frame) = stack.last_mut() else {
            return;
        };
        let moved = frame
            .constraint_modes
            .named
            .iter()
            .filter(|(identity, _)| identity.relation == *from)
            .map(|(identity, deferred)| (identity.clone(), *deferred))
            .collect::<Vec<_>>();
        for (old_identity, deferred) in moved {
            frame.constraint_modes.named.remove(&old_identity);
            frame.constraint_modes.named.insert(
                ConstraintIdentity {
                    relation: to.clone(),
                    name: old_identity.name,
                    object_id: old_identity.object_id,
                },
                deferred,
            );
        }
        let old_table = self.row_locks.table_key(&from.qualified_name());
        let new_table = self.row_locks.table_key(&to.qualified_name());
        for check in &mut frame.deferred_foreign_key_checks {
            if check.constraint.relation == *from {
                check.constraint.relation = to.clone();
            }
            if check.firing_relation == *from {
                check.firing_relation = to.clone();
            }
            if let Some(row) = &mut check.row {
                if row.table == old_table {
                    row.table = new_table;
                }
            }
        }
    }

    pub(crate) fn prune_constraint_modes(&self) -> Result<(), SQLError> {
        let live = crate::sql::runtime_constraints(self)?
            .into_iter()
            .map(|constraint| constraint.identity)
            .collect::<Vec<_>>();
        let live_relations = self
            .table_names()
            .map_err(|error| SQLError::Internal(format!("read live constraint tables: {error}")))?
            .into_iter()
            .map(|table| {
                RelationIdentity::from_legacy_name(&table).map_err(|error| {
                    SQLError::Internal(format!("decode live constraint table '{table}': {error}"))
                })
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if let Some(frame) = self.session.transactions.lock().last_mut() {
            let mut reconciled = std::collections::BTreeMap::new();
            for (identity, deferred) in std::mem::take(&mut frame.constraint_modes.named) {
                if let Some(current) =
                    find_live_constraint_identity(&live, &live_relations, &identity)
                {
                    reconciled.insert(current.clone(), deferred);
                }
            }
            frame.constraint_modes.named = reconciled;
            frame.deferred_foreign_key_checks.retain_mut(|check| {
                let Some(current) =
                    find_live_constraint_identity(&live, &live_relations, &check.constraint)
                else {
                    return false;
                };
                let previous_relation = check.constraint.relation.clone();
                check.constraint = current.clone();
                if previous_relation != current.relation {
                    let old_table = self
                        .row_locks
                        .table_key(&previous_relation.qualified_name());
                    let new_table = self.row_locks.table_key(&current.relation.qualified_name());
                    if check.firing_relation == previous_relation {
                        check.firing_relation = current.relation.clone();
                    }
                    if let Some(row) = &mut check.row {
                        if row.table == old_table {
                            row.table = new_table;
                        }
                    }
                }
                true
            });
        }
        Ok(())
    }

    pub(crate) fn forget_named_constraint_mode(&self, identity: &ConstraintIdentity) {
        if let Some(frame) = self.session.transactions.lock().last_mut() {
            frame
                .constraint_modes
                .named
                .retain(|candidate, _| !constraint_identities_match(candidate, identity));
        }
    }

    pub(crate) fn relation_has_pending_trigger_events(&self, relation: &RelationIdentity) -> bool {
        self.session
            .transactions
            .lock()
            .last()
            .is_some_and(|frame| {
                frame
                    .deferred_foreign_key_checks
                    .iter()
                    .any(|check| check.firing_relation == *relation)
            })
    }

    pub(crate) fn ensure_no_pending_trigger_events(
        &self,
        table: &str,
        command: &str,
    ) -> Result<(), SQLError> {
        let relation = RelationIdentity::from_legacy_name(table).map_err(|error| {
            SQLError::Internal(format!("decode pending-event relation '{table}': {error}"))
        })?;
        if self.relation_has_pending_trigger_events(&relation) {
            return Err(SQLError::Routine {
                sqlstate: "55006".into(),
                message: format!(
                    "cannot {command} \"{}\" because it has pending trigger events",
                    relation.name
                ),
            });
        }
        Ok(())
    }

    pub(crate) fn forget_constraint_transaction_relation(&self, relation: &RelationIdentity) {
        let table = self.row_locks.table_key(&relation.qualified_name());
        if let Some(frame) = self.session.transactions.lock().last_mut() {
            frame
                .constraint_modes
                .named
                .retain(|identity, _| identity.relation != *relation);
            frame.deferred_foreign_key_checks.retain(|check| {
                check.constraint.relation != *relation
                    && check.firing_relation != *relation
                    && check.row.is_none_or(|row| row.table != table)
            });
        }
    }

    pub(super) fn validate_deferred_constraints_before_commit(
        &self,
        stack: &mut Vec<TransactionFrame>,
        nested: bool,
    ) -> Result<(), SQLError> {
        if nested {
            return Ok(());
        }
        let validation = {
            let frame = stack
                .last()
                .ok_or_else(|| SQLError::Internal("COMMIT without an open transaction".into()))?;
            if frame.deferred_foreign_key_checks.is_empty() {
                return Ok(());
            }
            crate::sql::dml::validate_deferred_foreign_key_checks(
                self,
                &frame.deferred_foreign_key_checks,
                None,
            )
        };
        if let Err(validation_error) = validation {
            return Err(match self.rollback_transaction_frame(stack) {
                Ok(()) => validation_error,
                Err(rollback_error) => SQLError::Internal(format!(
                    "{validation_error}; deferred constraint rollback also failed: {rollback_error}"
                )),
            });
        }
        Ok(())
    }
}
