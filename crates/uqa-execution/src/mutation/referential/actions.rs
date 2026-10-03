//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The deferred `NO ACTION` checks that a delete or an update of a referenced row leaves for its transaction. A foreign key that is not deferred is checked, and its referential action taken, by the internal triggers that the row's change queues (`super::checks`).

use super::{
    foreign_key_comparison_types, foreign_key_lookup_values, period_foreign_key_coverage,
    referencing_rows, referrers_to_for_actions, BTreeSet, DocId, Document, ForeignKey,
    ForeignKeyAction, PhysicalDocumentIdentity, ReferentialContext, SQLError, Value,
};

/// Leave for the transaction the checks of the referenced keys that an update of the row `old_doc` of `table` to `new_doc` removes, under a deferred `NO ACTION` foreign key: the key's event, and each row that references the key, which the transaction checks again when the constraint becomes immediate. `referenced_relation` is the relation whose constraints the change fires: the row's table, or the relation an `UPDATE` names when the row moves to another partition.
pub fn defer_updated_key_checks<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    table: &str,
    referenced_relation: &str,
    parent_doc_id: DocId,
    old_doc: &Document,
    new_doc: &Document,
) -> Result<(), SQLError> {
    for (ref_table, fk) in referrers_to_for_actions(context.constraints.referrers, table)? {
        if fk.on_update != ForeignKeyAction::NoAction {
            continue;
        }
        let old_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| old_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        let new_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|c| new_doc.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        if old_values == new_values || old_values.iter().any(|v| matches!(v, Value::Null)) {
            continue;
        }
        let firing = uqa_sql::schema::referenced_partitions::firing_constraint(
            context.constraints.partitions.catalog,
            table,
            (referenced_relation != table).then_some(referenced_relation),
            &fk,
        )?;
        if !context
            .constraints
            .transactions
            .referenced_key_is_deferred(&ref_table, &fk, firing.derived)?
        {
            continue;
        }
        let comparison =
            foreign_key_comparison_types(context.constraints.partitions.catalog, &ref_table, &fk)?;
        let expected = comparison.normalize(old_values)?;
        context.deferrals.defer_foreign_key_parent_event(
            &ref_table,
            firing.relation,
            &fk,
            firing.derived,
        )?;
        if fk.period {
            let parent = PhysicalDocumentIdentity {
                table: table.to_string(),
                doc_id: parent_doc_id,
            };
            defer_period_key_checks(
                context,
                PeriodKeyChecks {
                    constraint_table: &ref_table,
                    firing_relation: firing.relation,
                    foreign_key: &fk,
                    derived: firing.derived,
                    expected: &expected,
                    root_deletes: &BTreeSet::new(),
                },
                std::slice::from_ref(&parent),
                Some((&parent, new_doc)),
            )?;
            continue;
        }
        for (child, _child_doc) in referencing_rows(
            context,
            &ref_table,
            &fk,
            &comparison,
            &expected,
            ForeignKeyAction::NoAction,
        )? {
            context.deferrals.defer_foreign_key_check(
                &ref_table,
                firing.relation,
                &child.table,
                child.doc_id,
                &fk,
                firing.derived,
            )?;
        }
    }
    Ok(())
}

/// Leave for the transaction the checks of the referenced keys that a delete of the row `parent_document` of `parent_table` removes, under a deferred `NO ACTION` foreign key: the key's event, and each row that references the key, which the transaction checks again when the constraint becomes immediate. A row in `root_deletes`, which the same statement deletes, needs no check.
pub fn defer_deleted_key_checks<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    parent_table: &str,
    parent_doc_id: DocId,
    parent_document: &Document,
    root_deletes: &BTreeSet<(String, DocId)>,
) -> Result<(), SQLError> {
    for (ref_table, fk) in referrers_to_for_actions(context.constraints.referrers, parent_table)? {
        if fk.on_delete != ForeignKeyAction::NoAction {
            continue;
        }
        let key_values: Vec<Value> = fk
            .ref_columns
            .iter()
            .map(|column| parent_document.get(column).cloned().unwrap_or(Value::Null))
            .collect();
        if key_values.iter().any(|value| matches!(value, Value::Null)) {
            continue;
        }
        let firing = uqa_sql::schema::referenced_partitions::firing_constraint(
            context.constraints.partitions.catalog,
            parent_table,
            None,
            &fk,
        )?;
        if !context
            .constraints
            .transactions
            .referenced_key_is_deferred(&ref_table, &fk, firing.derived)?
        {
            continue;
        }
        let comparison =
            foreign_key_comparison_types(context.constraints.partitions.catalog, &ref_table, &fk)?;
        let expected = comparison.normalize(key_values)?;
        context.deferrals.defer_foreign_key_parent_event(
            &ref_table,
            parent_table,
            &fk,
            firing.derived,
        )?;
        if fk.period {
            let mut excluded_parents = root_deletes
                .iter()
                .map(|(table, doc_id)| PhysicalDocumentIdentity {
                    table: table.clone(),
                    doc_id: *doc_id,
                })
                .collect::<Vec<_>>();
            let parent_identity = PhysicalDocumentIdentity {
                table: parent_table.to_string(),
                doc_id: parent_doc_id,
            };
            if !excluded_parents.contains(&parent_identity) {
                excluded_parents.push(parent_identity);
            }
            defer_period_key_checks(
                context,
                PeriodKeyChecks {
                    constraint_table: &ref_table,
                    firing_relation: parent_table,
                    foreign_key: &fk,
                    derived: firing.derived,
                    expected: &expected,
                    root_deletes,
                },
                &excluded_parents,
                None,
            )?;
            continue;
        }
        for (child, _child_document) in referencing_rows(
            context,
            &ref_table,
            &fk,
            &comparison,
            &expected,
            ForeignKeyAction::NoAction,
        )? {
            if root_deletes.contains(&(child.table.clone(), child.doc_id)) {
                continue;
            }
            context.deferrals.defer_foreign_key_check(
                &ref_table,
                parent_table,
                &child.table,
                child.doc_id,
                &fk,
                firing.derived,
            )?;
        }
    }
    Ok(())
}

/// The removed period key whose referencing rows a deferred `NO ACTION` foreign key checks again when it becomes immediate.
struct PeriodKeyChecks<'a> {
    constraint_table: &'a str,
    firing_relation: &'a str,
    foreign_key: &'a ForeignKey,
    derived: Option<&'a uqa_sql::ast::ReferencedPartitionConstraint>,
    expected: &'a [Value],
    /// The rows the same statement deletes, which need no check.
    root_deletes: &'a BTreeSet<(String, DocId)>,
}

/// Leave for the transaction a check of each row that references the leading values of a removed period key over a period the remaining referenced rows no longer cover. `excluded_parents` holds the referenced rows the statement removes, and `replacement` the row an update writes in place of one.
fn defer_period_key_checks<S: Clone + 'static>(
    context: &ReferentialContext<'_, S>,
    key: PeriodKeyChecks<'_>,
    excluded_parents: &[PhysicalDocumentIdentity],
    replacement: Option<(&PhysicalDocumentIdentity, &Document)>,
) -> Result<(), SQLError> {
    let snapshot = super::snapshots::ReferenceSnapshot::new(context)?;
    let ordinary_len = key.expected.len().saturating_sub(1);
    for physical_table in uqa_sql::semantics::partition::foreign_key_scan_tables(
        context.constraints.partitions.catalog,
        key.constraint_table,
    )? {
        let rows = snapshot.table(&physical_table)?;
        for child_id in rows.doc_ids()? {
            if key
                .root_deletes
                .contains(&(physical_table.clone(), child_id))
            {
                continue;
            }
            let Some(child_document) = rows.document(child_id)? else {
                continue;
            };
            let Some(child_lookup) = foreign_key_lookup_values(
                context.constraints.partitions.catalog,
                &physical_table,
                key.foreign_key,
                &child_document,
            )?
            else {
                continue;
            };
            if child_lookup.values[..ordinary_len] != key.expected[..ordinary_len] {
                continue;
            }
            if period_foreign_key_coverage(
                context.constraints,
                key.foreign_key,
                &child_lookup.values,
                excluded_parents,
                replacement,
            )?
            .0
            {
                continue;
            }
            context.deferrals.defer_foreign_key_check(
                key.constraint_table,
                key.firing_relation,
                &physical_table,
                child_id,
                key.foreign_key,
                key.derived,
            )?;
        }
    }
    Ok(())
}
