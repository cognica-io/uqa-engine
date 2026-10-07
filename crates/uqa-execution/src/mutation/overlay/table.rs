//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Atomic row/key publication, namespace-separated exact indexes and encrypted spill.

use super::{
    keys, resource_error, spilled, storage_error, Arc, BudgetedMap, BudgetedVec, CommandExactIndex,
    CommandStoredDocument, CommandTableOverlay, DocId, FieldSet, KeyFields, KeyKind, SQLError,
    StorageReadControl,
};

type Indexes = BudgetedMap<FieldSet, CommandExactIndex>;

fn sources<'a>(
    columns: &'a Indexes,
    expressions: &'a Indexes,
) -> impl Iterator<Item = (KeyFields<'a>, &'a CommandExactIndex)> {
    columns
        .iter()
        .map(|(fields, index)| {
            (
                KeyFields {
                    kind: KeyKind::Columns,
                    fields,
                },
                index,
            )
        })
        .chain(expressions.iter().map(|(fields, index)| {
            (
                KeyFields {
                    kind: KeyKind::Expressions,
                    fields,
                },
                index,
            )
        }))
}

impl CommandTableOverlay {
    pub(super) fn indexes(&self, kind: KeyKind) -> &Indexes {
        match kind {
            KeyKind::Columns => &self.exact_indexes,
            KeyKind::Expressions => &self.expression_indexes,
        }
    }
    fn indexes_mut(&mut self, kind: KeyKind) -> &mut Indexes {
        match kind {
            KeyKind::Columns => &mut self.exact_indexes,
            KeyKind::Expressions => &mut self.expression_indexes,
        }
    }
}

impl CommandTableOverlay {
    pub(super) fn stage(
        &mut self,
        id: DocId,
        document: Option<CommandStoredDocument>,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        if self.rows.needs_room(control) {
            self.spill(control)?;
        }
        let has_fallible_comparison = document.as_ref().is_some_and(|document| {
            document
                .fields
                .values()
                .chain(document.index_values.iter().flat_map(|keys| keys.values()))
                .any(uqa_sql::expr::value_comparison_can_fail)
        });
        // The memory tier's indexes hold its own rows; a spilled version that this row replaces keeps its entries in the spilled tier, where the row in memory shadows them.
        let previous = self.rows.memory.get(&id).and_then(Option::as_ref);
        let mut updates = BudgetedVec::new(control.memory());
        for (fields, index) in sources(&self.exact_indexes, &self.expression_indexes) {
            control.check().map_err(resource_error)?;
            let change = index.prepare(id, previous, document.as_ref(), fields, control)?;
            updates.push(change).map_err(resource_error)?;
        }
        control.check().map_err(resource_error)?;
        self.rows.insert(id, document).map_err(storage_error)?;
        // Every fallible operation precedes publication. Borrow each prepared update in the same immutable field-set order used above; no field-name copies or lookup allocations are needed here.
        self.has_fallible_comparison |= has_fallible_comparison;
        let mut changes = updates.iter_mut();
        for kind in [KeyKind::Columns, KeyKind::Expressions] {
            self.indexes_mut(kind).for_each_mut(|_, index| {
                index.apply(id, changes.next().expect("prepared index change").take());
            });
        }
        Ok(())
    }

    /// Move every row in memory, with its exact index entries, into the spilled tier. Failure leaves both tiers unchanged.
    pub(super) fn spill(&mut self, control: &StorageReadControl) -> Result<(), SQLError> {
        if self.rows.spilled.is_none() {
            self.rows.spilled = Some(spilled::SpilledRows::new(control).map_err(storage_error)?);
        }
        let memory = &self.rows.memory;
        let indexes = &self.exact_indexes;
        let expressions = &self.expression_indexes;
        let spilled = self.rows.spilled.as_mut().expect("a spilled tier");
        let previous = Arc::clone(spilled.view());
        spilled.transact(control, |writer| {
            let mut batch = Vec::new();
            let mut counts = std::collections::BTreeMap::new();
            for (&id, row) in memory {
                control.check().map_err(resource_error)?;
                // The entries of the version this row replaces leave the spilled tier with it.
                let replaced = if indexes.is_empty() && expressions.is_empty() {
                    None
                } else {
                    spilled::row(&previous, id, control)
                        .map_err(storage_error)?
                        .flatten()
                };
                batch.push((
                    spilled::row_key(id),
                    row.as_ref()
                        .map(spilled::encode_row)
                        .transpose()
                        .map_err(storage_error)?,
                ));
                for (fields, index) in sources(indexes, expressions) {
                    let ordinal = index.ordinal();
                    let old = replaced
                        .as_ref()
                        .map(|old| keys::document_key(old, fields, control))
                        .transpose()?;
                    let new = row
                        .as_ref()
                        .map(|row| keys::document_key(row, fields, control))
                        .transpose()?;
                    if old != new {
                        if let Some(old) = &old {
                            batch.push((
                                spilled::index_key(ordinal, old.bytes(), id)
                                    .map_err(storage_error)?,
                                None,
                            ));
                            *counts
                                .entry(
                                    spilled::key_record(ordinal, old.bytes())
                                        .map_err(storage_error)?,
                                )
                                .or_insert(0) -= 1;
                        }
                        if let Some(new) = &new {
                            *counts
                                .entry(
                                    spilled::key_record(ordinal, new.bytes())
                                        .map_err(storage_error)?,
                                )
                                .or_insert(0) += 1;
                        }
                    }
                    if let (Some(row), Some(new)) = (row, &new) {
                        batch.push((
                            spilled::index_key(ordinal, new.bytes(), id).map_err(storage_error)?,
                            Some(spilled::index_value(fields.complete(row)?)),
                        ));
                    }
                }
                if batch.len() >= spilled::PAGE_RECORDS {
                    writer.write(&mut batch, &mut counts)?;
                }
            }
            writer.write(&mut batch, &mut counts)
        })?;
        self.rows.clear_memory();
        self.exact_indexes.for_each_mut(|_, index| index.clear());
        self.expression_indexes
            .for_each_mut(|_, index| index.clear());
        Ok(())
    }

    /// Build the exact index of `fields` when the table has none: over the rows in memory, and as entries of the spilled tier over its rows.
    pub(super) fn prepare_index(
        &mut self,
        kind: KeyKind,
        fields: &FieldSet,
        control: &StorageReadControl,
    ) -> Result<(), SQLError> {
        if self.indexes(kind).contains_key(fields.values()) {
            return Ok(());
        }
        let names = FieldSet::copy(fields.values().iter().map(String::as_str), control)?;
        let ordinal = self.next_ordinal;
        self.next_ordinal = ordinal.checked_add(1).ok_or_else(|| {
            SQLError::Internal("command exact index ordinals are exhausted".into())
        })?;
        let index = CommandExactIndex::build(
            &self.rows.memory,
            KeyFields {
                kind,
                fields: &names,
            },
            ordinal,
            control,
        )?;
        if let Some(spilled) = self.rows.spilled.as_mut() {
            let view = Arc::clone(spilled.view());
            spilled.transact(control, |writer| {
                let mut after = None;
                let mut batch = Vec::new();
                let mut counts = std::collections::BTreeMap::new();
                loop {
                    let page = spilled::row_page(&view, after, control).map_err(storage_error)?;
                    for (id, row) in page.rows.iter() {
                        let Some(row) = row else { continue };
                        let key = keys::document_key(
                            row,
                            KeyFields {
                                kind,
                                fields: &names,
                            },
                            control,
                        )?;
                        batch.push((
                            spilled::index_key(ordinal, key.bytes(), *id).map_err(storage_error)?,
                            Some(spilled::index_value(
                                KeyFields {
                                    kind,
                                    fields: &names,
                                }
                                .complete(row)?,
                            )),
                        ));
                        *counts
                            .entry(
                                spilled::key_record(ordinal, key.bytes()).map_err(storage_error)?,
                            )
                            .or_insert(0) += 1;
                    }
                    writer.write(&mut batch, &mut counts)?;
                    match page.resume {
                        Some(resume) => after = Some(resume),
                        None => return Ok(()),
                    }
                }
            })?;
        }
        let entry = self
            .indexes_mut(kind)
            .prepare_entry(names, index)
            .map_err(resource_error)?;
        control.check().map_err(resource_error)?;
        self.indexes_mut(kind).insert_prepared(entry);
        Ok(())
    }
}
