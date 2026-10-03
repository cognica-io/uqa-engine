//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The foreign key checks and referential actions that `PostgreSQL` queues as internal AFTER ROW triggers of a written row and runs once the statement has written its rows: `RI_FKey_check_ins` and `RI_FKey_check_upd` on a referencing row, the `NO ACTION` and `RESTRICT` checks of a referenced key that a delete or an update removed, and the `CASCADE`, `SET NULL` and `SET DEFAULT` actions that write the rows referencing such a key. A check therefore sees every row of the statement: a row may reference a row that the same statement writes later, and a later row's unique or NOT NULL violation is reported first.

use std::sync::Arc;

use uqa_core::{DocId, Value};
use uqa_sql::{
    ast::{ForeignKey, ForeignKeyAction},
    semantics::{
        foreign_keys::{
            foreign_key_comparison_types, foreign_key_lookup_values, foreign_key_relation_name,
        },
        referential::referrers_to_for_actions,
    },
    SQLError,
};
use uqa_storage::document_store::Document;

use super::ReferentialContext;
use crate::mutation::constraints::{period::period_foreign_key_coverage, ConstraintContext};

/// A foreign key check or referential action queued with the AFTER ROW triggers of a written row.
#[derive(Debug, Clone)]
pub struct ForeignKeyCheck {
    /// The position of the foreign key among the foreign keys the check is drawn from, which orders the internal triggers of one row as their object identifiers do.
    ordinal: usize,
    kind: CheckKind,
}

#[derive(Debug, Clone)]
enum CheckKind {
    Referencing(ReferencingRow),
    Referenced(Box<ReferencedKey>),
    Action(Box<super::cascades::ReferentialAction>),
}

/// The row that `table` holds at `doc_id` references a row through `foreign_key`, unless a later change of the statement removed it.
#[derive(Debug, Clone)]
struct ReferencingRow {
    table: String,
    doc_id: DocId,
    foreign_key: Arc<ForeignKey>,
}

/// A delete or an update of a row of `relation` removed the referenced key `key`, which no row of `constraint_table` may still reference: under `NO ACTION` unless another row holds the key again, under `RESTRICT` whatever row holds it.
#[derive(Debug, Clone)]
struct ReferencedKey {
    constraint_table: String,
    /// The relation whose internal trigger fires: the partition that held the row, or the table an `UPDATE` named for a row it moved to another partition.
    relation: String,
    /// The constraint the check reports: the foreign key's, or the one it derives on `relation`.
    constraint_name: String,
    foreign_key: Arc<ForeignKey>,
    key: Vec<Value>,
    restrict: bool,
}

/// The prefix of an internal trigger's name that checks a referencing row.
const CHECK_TRIGGER: &str = "RI_ConstraintTrigger_c_";
/// The prefix of an internal trigger's name that acts on a referenced key.
const ACTION_TRIGGER: &str = "RI_ConstraintTrigger_a_";

impl ForeignKeyCheck {
    fn trigger_prefix(&self) -> &'static str {
        match self.kind {
            CheckKind::Referencing(_) => CHECK_TRIGGER,
            CheckKind::Referenced(_) | CheckKind::Action(_) => ACTION_TRIGGER,
        }
    }

    /// Whether the check runs before the user trigger `name` of the same row. A row's AFTER triggers fire in the order of their names, and an internal trigger's name is its prefix followed by its object identifier, so it precedes every name that sorts after the prefix; a user trigger whose name begins with the prefix itself follows it.
    pub fn precedes_trigger(&self, name: &str) -> bool {
        self.trigger_prefix().as_bytes() <= name.as_bytes()
    }

    /// The checks of one row in the order of their internal triggers' names: the actions on referenced keys before the checks of referencing rows, each in the order of the foreign keys.
    pub fn sort(checks: &mut [Self]) {
        checks.sort_by(|left, right| {
            (left.trigger_prefix(), left.ordinal).cmp(&(right.trigger_prefix(), right.ordinal))
        });
    }
}

fn changed(columns: &[String], old: &Document, new: &Document) -> bool {
    columns
        .iter()
        .any(|column| old.get(column) != new.get(column))
}

/// The checks of the referencing row that `table` holds at `doc_id` after an insert, or after an update when `old` holds the row it replaced, as `RI_FKey_check_ins` and `RI_FKey_check_upd` are queued: an update checks only a foreign key whose columns it changed, a key that NULLs exempt needs no check, and a deferred foreign key waits for its transaction instead.
pub fn referencing_checks(
    context: ConstraintContext<'_>,
    table: &str,
    doc_id: DocId,
    new: &Document,
    old: Option<&Document>,
) -> Result<Vec<ForeignKeyCheck>, SQLError> {
    if context.referrers.session_replication_role_is_replica() {
        return Ok(Vec::new());
    }
    let foreign_keys = context
        .catalog
        .try_foreign_keys(table)
        .map_err(|error| SQLError::Internal(format!("read foreign keys of `{table}`: {error}")))?;
    let mut checks = Vec::new();
    for (ordinal, foreign_key) in foreign_keys.into_iter().enumerate() {
        if !foreign_key.enforced
            || old.is_some_and(|old| !changed(&foreign_key.local_columns, old, new))
        {
            continue;
        }
        let nulls = foreign_key
            .local_columns
            .iter()
            .filter(|column| matches!(new.get(*column).unwrap_or(&Value::Null), Value::Null))
            .count();
        let exempt = match foreign_key.match_type {
            uqa_sql::ast::ForeignKeyMatch::Simple => nulls > 0,
            uqa_sql::ast::ForeignKeyMatch::Full => nulls == foreign_key.local_columns.len(),
        };
        if exempt
            || context
                .transactions
                .foreign_key_is_deferred(table, &foreign_key)?
        {
            continue;
        }
        checks.push(ForeignKeyCheck {
            ordinal,
            kind: CheckKind::Referencing(ReferencingRow {
                table: table.to_string(),
                doc_id,
                foreign_key: Arc::new(foreign_key),
            }),
        });
    }
    Ok(checks)
}

/// Which of the foreign keys that reference a row's partition or its ancestors a change of the row fires.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReferencedKeys {
    /// Every one: an ordinary delete or update.
    All,
    /// Those that reference the partition itself: the delete half of a row's move to another partition, whose cloned triggers of an ancestor's foreign keys do not fire (`AfterTriggerSaveEvent`).
    OfPartition,
    /// Those that reference an ancestor: the update a row's move to another partition fires on the table the `UPDATE` named (`ExecCrossPartitionUpdateForeignKey`).
    OfAncestors,
}

/// The checks and actions of the referenced keys that a delete of the row `old` of `table` removes, or an update that replaces it with `new`, as `RI_FKey_noaction_*`, `RI_FKey_restrict_*`, `RI_FKey_cascade_*`, `RI_FKey_setnull_*` and `RI_FKey_setdefault_*` are queued. An update queues them only when it changes the key (`RI_FKey_pk_upd_check_required`), a check of a key holding a NULL finds no referencing row, and a deferred `NO ACTION` key waits for its transaction. `moved_through` names the table an `UPDATE` named for a row it moved to another partition, whose update fires the foreign keys of the partition's ancestors.
pub fn referenced_checks(
    context: ConstraintContext<'_>,
    table: &str,
    old: &Document,
    new: Option<&Document>,
    moved_through: Option<&str>,
) -> Result<Vec<ForeignKeyCheck>, SQLError> {
    let keys = if moved_through.is_some() {
        ReferencedKeys::OfAncestors
    } else {
        ReferencedKeys::All
    };
    collect_referenced_checks(context, table, old, new, moved_through, keys)
}

/// The checks and actions that the delete half of a move of the row `old` out of the partition `table` fires: those of the foreign keys that reference the partition itself.
pub fn moved_row_delete_checks(
    context: ConstraintContext<'_>,
    table: &str,
    old: &Document,
) -> Result<Vec<ForeignKeyCheck>, SQLError> {
    collect_referenced_checks(context, table, old, None, None, ReferencedKeys::OfPartition)
}

fn collect_referenced_checks(
    context: ConstraintContext<'_>,
    table: &str,
    old: &Document,
    new: Option<&Document>,
    moved_through: Option<&str>,
    keys: ReferencedKeys,
) -> Result<Vec<ForeignKeyCheck>, SQLError> {
    let resolve = |name: &str| {
        context
            .partitions
            .catalog
            .try_resolve_table_name(name)
            .map_err(SQLError::Internal)
            .map(|resolved| resolved.unwrap_or_else(|| name.to_string()))
    };
    let partition = if keys == ReferencedKeys::All {
        String::new()
    } else {
        resolve(table)?
    };
    let mut checks = Vec::new();
    for (ordinal, (constraint_table, foreign_key)) in
        referrers_to_for_actions(context.referrers, table)?
            .into_iter()
            .enumerate()
    {
        if keys != ReferencedKeys::All
            && (resolve(&foreign_key.ref_table)? == partition)
                != (keys == ReferencedKeys::OfPartition)
        {
            continue;
        }
        let action = if new.is_some() {
            foreign_key.on_update
        } else {
            foreign_key.on_delete
        };
        if !foreign_key.enforced
            || new.is_some_and(|new| !changed(&foreign_key.ref_columns, old, new))
        {
            continue;
        }
        let key = foreign_key
            .ref_columns
            .iter()
            .map(|column| old.get(column).cloned().unwrap_or(Value::Null))
            .collect::<Vec<_>>();
        let null_key = key.iter().any(|value| matches!(value, Value::Null));
        if matches!(
            action,
            ForeignKeyAction::Cascade | ForeignKeyAction::SetNull | ForeignKeyAction::SetDefault
        ) {
            // An update of a key holding a NULL needs no action; a delete queues one whatever the key.
            if new.is_some() && null_key {
                continue;
            }
            let new_key = new.map(|new| {
                foreign_key
                    .ref_columns
                    .iter()
                    .map(|column| new.get(column).cloned().unwrap_or(Value::Null))
                    .collect()
            });
            checks.push(ForeignKeyCheck {
                ordinal,
                kind: CheckKind::Action(Box::new(super::cascades::ReferentialAction {
                    constraint_table,
                    foreign_key: Arc::new(foreign_key),
                    key,
                    new_key,
                })),
            });
            continue;
        }
        if null_key {
            continue;
        }
        let firing = uqa_sql::schema::referenced_partitions::firing_constraint(
            context.partitions.catalog,
            table,
            moved_through,
            &foreign_key,
        )?;
        let restrict = action == ForeignKeyAction::Restrict;
        if !restrict
            && context.transactions.referenced_key_is_deferred(
                &constraint_table,
                &foreign_key,
                firing.derived,
            )?
        {
            continue;
        }
        let relation = firing.relation.to_string();
        let constraint_name = firing.name(&foreign_key).to_string();
        checks.push(ForeignKeyCheck {
            ordinal,
            kind: CheckKind::Referenced(Box::new(ReferencedKey {
                constraint_table,
                relation,
                constraint_name,
                foreign_key: Arc::new(foreign_key),
                key,
                restrict,
            })),
        });
    }
    Ok(checks)
}

/// Run a queued check or take a queued action once the statement has written its rows. An action queues the events of the rows it writes in `queue`.
pub fn run_foreign_key_check<S: Clone + 'static>(
    context: &crate::mutation::statement::MutationExecutionContext<'_, S>,
    check: &ForeignKeyCheck,
    queue: &crate::mutation::triggers::queue::AfterTriggerQueue,
) -> Result<(), SQLError> {
    let referential = &context.preparation.referential;
    match &check.kind {
        CheckKind::Referencing(row) => check_referencing_row(referential.constraints, row),
        CheckKind::Referenced(key) => check_referenced_key(referential, key),
        CheckKind::Action(action) => {
            super::cascades::run_referential_action(context, action, queue)
        }
    }
}

/// `RI_FKey_check`: a row that the statement removed again is not checked, and the row as it stands must reference an existing row, which the check locks `FOR KEY SHARE`.
fn check_referencing_row(
    context: ConstraintContext<'_>,
    row: &ReferencingRow,
) -> Result<(), SQLError> {
    let Some(document) = context.reads.get_document(&row.table, row.doc_id)? else {
        return Ok(());
    };
    let Some(lookup) = foreign_key_lookup_values(
        context.partitions.catalog,
        &row.table,
        &row.foreign_key,
        &document,
    )?
    else {
        return Ok(());
    };
    crate::mutation::constraints::require_foreign_key_parent(
        context,
        &row.table,
        &row.foreign_key,
        &lookup,
        &document,
    )
}

/// `ri_restrict`: under `NO ACTION` a key that another row of the referenced table holds again is satisfied, and otherwise no row may reference the removed key. The check reads the foreign key's table `FOR KEY SHARE`, which takes its `ROW SHARE` lock.
fn check_referenced_key<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    key: &ReferencedKey,
) -> Result<(), SQLError> {
    let foreign_key = key.foreign_key.as_ref();
    context.locking.session.lock_relation(
        &key.constraint_table,
        crate::row_locks::RelationLockMode::RowShare,
    )?;
    if !key.restrict
        && !foreign_key.period
        && crate::mutation::constraints::find_exact_foreign_key_parent(
            context.constraints,
            foreign_key,
            &key.key,
        )?
        .is_some()
    {
        return Ok(());
    }
    let comparison = foreign_key_comparison_types(
        context.constraints.partitions.catalog,
        &key.constraint_table,
        foreign_key,
    )?;
    let expected = comparison.normalize(key.key.clone())?;
    let referenced = if foreign_key.period {
        period_key_referenced(context, key, &expected)?
    } else {
        !super::referencing_rows(
            context,
            &key.constraint_table,
            foreign_key,
            &comparison,
            &expected,
            if key.restrict {
                ForeignKeyAction::Restrict
            } else {
                ForeignKeyAction::NoAction
            },
        )?
        .is_empty()
    };
    if referenced {
        return Err(referenced_key_violation(context.constraints, key)?);
    }
    Ok(())
}

/// Whether a row of the foreign key's table holds the removed key's leading values over a period that the referenced table no longer covers.
fn period_key_referenced<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    key: &ReferencedKey,
    expected: &[Value],
) -> Result<bool, SQLError> {
    let foreign_key = key.foreign_key.as_ref();
    let ordinary = expected.len().saturating_sub(1);
    let snapshot = super::snapshots::ReferenceSnapshot::new(context)?;
    for table in uqa_sql::semantics::partition::foreign_key_scan_tables(
        context.constraints.partitions.catalog,
        &key.constraint_table,
    )? {
        let rows = snapshot.table(&table)?;
        for doc_id in rows.doc_ids()? {
            let Some(document) = rows.document(doc_id)? else {
                continue;
            };
            let Some(lookup) = foreign_key_lookup_values(
                context.constraints.partitions.catalog,
                &table,
                foreign_key,
                &document,
            )?
            else {
                continue;
            };
            if lookup.values[..ordinary] != expected[..ordinary] {
                continue;
            }
            if !period_foreign_key_coverage(
                context.constraints,
                foreign_key,
                &lookup.values,
                &[],
                None,
            )?
            .0
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// `ri_ReportViolation` for a removed key that a row still references: `23503` under `NO ACTION`, `23001` under `RESTRICT`, with the key unless the current role may not read its columns.
fn referenced_key_violation(
    context: ConstraintContext<'_>,
    key: &ReferencedKey,
) -> Result<SQLError, SQLError> {
    let relation = foreign_key_relation_name(&key.relation);
    let referencing = foreign_key_relation_name(&key.constraint_table);
    let columns = &key.foreign_key.ref_columns;
    let shown = crate::mutation::constraints::foreign_key_key(
        context,
        &key.relation,
        columns,
        &key.relation,
        columns,
        &key.key,
    )?;
    let (sqlstate, message, reference) = if key.restrict {
        (
            "23001",
            format!(
                "update or delete on table \"{relation}\" violates RESTRICT setting of foreign key constraint \"{}\" on table \"{referencing}\"",
                key.constraint_name
            ),
            "is referenced",
        )
    } else {
        (
            "23503",
            format!(
                "update or delete on table \"{relation}\" violates foreign key constraint \"{}\" on table \"{referencing}\"",
                key.constraint_name
            ),
            "is still referenced",
        )
    };
    Ok(SQLError::Diagnostic {
        sqlstate: sqlstate.into(),
        message,
        detail: Some(match shown {
            Some(shown) => format!("{shown} {reference} from table \"{referencing}\"."),
            None => format!("Key {reference} from table \"{referencing}\"."),
        }),
        hint: None,
    })
}
