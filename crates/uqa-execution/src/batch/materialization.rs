//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Final conversion from positional physical rows to named result rows.

use std::collections::HashMap;

mod selected;

pub(crate) use selected::SelectedRowMaterializer;

use super::{
    Arc, PhysicalRow, ResultRow, RowFragment, RowSchema, SmallVec, Value, INLINE_ROW_FRAGMENTS,
    NULL_SLOT,
};

struct ResultMaterializationPlan {
    entries: Box<[ResultMaterializationEntry]>,
    selected: SelectedRowMaterializer,
}

struct ResultMaterializationEntry {
    column: String,
    slot: usize,
}

/// Reuse the immutable schema's conversion plan for public result consumers.
pub(crate) struct ResultRowMaterializer {
    schema: RowSchema,
    plan: Option<ResultMaterializationPlan>,
}

impl ResultRowMaterializer {
    pub(crate) fn new(schema: RowSchema) -> Self {
        let plan = (!schema.is_identity_layout())
            .then(|| RowMaterializer::new(&schema).result_materialization_plan());
        Self { schema, plan }
    }

    pub(crate) fn matches_schema(&self, schema: &RowSchema) -> bool {
        self.schema.shares_index(schema)
    }

    pub(crate) fn materialize(&self, row: PhysicalRow) -> ResultRow {
        let materializer = RowMaterializer::new(&self.schema);
        match &self.plan {
            Some(plan) => materializer.materialize_remapped_result_row_with_plan(row, plan),
            None => materializer.materialize_identity_result_row(row),
        }
    }

    pub(crate) fn materialize_borrowed(&self, values: &[&Value]) -> ResultRow {
        let mut result = ResultRow::new();
        match &self.plan {
            Some(plan) => {
                for entry in &plan.entries {
                    result.insert(
                        entry.column.clone(),
                        values
                            .get(entry.slot)
                            .copied()
                            .cloned()
                            .unwrap_or(Value::Null),
                    );
                }
            }
            None => {
                for (column, value) in self.schema.columns().iter().zip(values) {
                    result.insert(column.clone(), (*value).clone());
                }
            }
        }
        result
    }

    pub(crate) fn extend(&self, output: &mut Vec<ResultRow>, rows: Vec<PhysicalRow>) {
        let materializer = RowMaterializer::new(&self.schema);
        match &self.plan {
            Some(plan) => output.extend(
                rows.into_iter()
                    .map(|row| materializer.materialize_remapped_result_row_with_plan(row, plan)),
            ),
            None => output.extend(
                rows.into_iter()
                    .map(|row| materializer.materialize_identity_result_row(row)),
            ),
        }
    }
}

pub(super) struct RowMaterializer<'a> {
    schema: &'a RowSchema,
}

impl<'a> RowMaterializer<'a> {
    pub(super) fn new(schema: &'a RowSchema) -> Self {
        Self { schema }
    }

    pub(super) fn materialize_result_row(&self, row: PhysicalRow) -> ResultRow {
        if self.schema.is_identity_layout() {
            return self.materialize_identity_result_row(row);
        }
        self.materialize_remapped_result_row(row)
    }

    pub(super) fn materialize_remapped_result_row(&self, row: PhysicalRow) -> ResultRow {
        let plan = self.result_materialization_plan();
        self.materialize_remapped_result_row_with_plan(row, &plan)
    }

    pub(super) fn materialize_remapped_result_rows(
        &self,
        rows: Vec<PhysicalRow>,
    ) -> Vec<ResultRow> {
        let plan = self.result_materialization_plan();
        rows.into_iter()
            .map(|row| self.materialize_remapped_result_row_with_plan(row, &plan))
            .collect()
    }

    fn result_materialization_plan(&self) -> ResultMaterializationPlan {
        let columns = self.schema.columns();
        let mut last_logical_by_label = HashMap::<&str, usize>::with_capacity(columns.len());
        for (logical, column) in columns.iter().enumerate() {
            last_logical_by_label.insert(column, logical);
        }
        let mut logical_order = last_logical_by_label.into_values().collect::<Vec<_>>();
        logical_order.sort_unstable_by(|left, right| columns[*left].cmp(&columns[*right]));
        let positions = logical_order.iter().copied().map(Some).collect::<Vec<_>>();
        let selected = SelectedRowMaterializer::new(self.schema, &positions);
        let entries = logical_order
            .into_iter()
            .map(|logical| ResultMaterializationEntry {
                column: columns[logical].clone(),
                slot: self.schema.layout_slots()[logical],
            })
            .collect::<Vec<_>>();
        ResultMaterializationPlan {
            entries: entries.into_boxed_slice(),
            selected,
        }
    }

    fn materialize_remapped_result_row_with_plan(
        &self,
        row: PhysicalRow,
        plan: &ResultMaterializationPlan,
    ) -> ResultRow {
        // A shared scan fragment cannot donate its values, so read only requested output slots instead of cloning an intermediate positional vector that is immediately dismantled.
        if row.fragments.len() == 1 && row.fragments[0].can_copy_directly() {
            return self.materialize_remapped_shared_fragment_row(&row.fragments[0], plan);
        }
        debug_assert_eq!(
            self.schema.physical_width(),
            row.fragments.iter().map(RowFragment::len).sum::<usize>()
        );
        // Missing positions stay NULL; selected transfer discards unused aliases.
        let mut result = ResultRow::new();
        for entry in &plan.entries {
            result.insert(entry.column.clone(), Value::Null);
        }
        plan.selected.materialize(row, |output, value| {
            *result
                .get_mut(&plan.entries[output].column)
                .expect("selected result label was initialized") = value;
        });
        result
    }

    fn materialize_remapped_shared_fragment_row(
        &self,
        fragment: &RowFragment,
        plan: &ResultMaterializationPlan,
    ) -> ResultRow {
        debug_assert_eq!(self.schema.physical_width(), fragment.len());
        let mut result = ResultRow::new();
        for entry in &plan.entries {
            let slot = entry.slot;
            let value = if slot == NULL_SLOT {
                Value::Null
            } else {
                fragment.get(slot).cloned().unwrap_or(Value::Null)
            };
            result.insert(entry.column.clone(), value);
        }
        result
    }

    pub(super) fn materialize_identity_result_row(&self, row: PhysicalRow) -> ResultRow {
        debug_assert_eq!(
            self.schema.len(),
            row.fragments.iter().map(RowFragment::len).sum::<usize>()
        );
        let mut columns = self.schema.columns().iter();
        let mut result = ResultRow::new();
        for fragment in row.fragments {
            fragment.materialize_into(&mut columns, &mut result);
        }
        result
    }
}

impl RowFragment {
    /// Insert this fragment directly into a named result row. Shared scan projections clone only the selected values; they do not allocate an intermediate positional row at the final materialization boundary.
    fn materialize_into<'a>(
        self,
        columns: &mut impl Iterator<Item = &'a String>,
        result: &mut ResultRow,
    ) {
        let (values, projection) = match self {
            Self::Constant { value, width } => {
                insert_values(columns, result, std::iter::repeat_n(value.clone(), width));
                return;
            }
            Self::Stored { values, projection } => (values, projection),
        };
        let Some(projection) = projection else {
            match Arc::try_unwrap(values) {
                Ok(values) => insert_values(columns, result, values),
                Err(values) => insert_values(columns, result, values.iter().cloned()),
            }
            return;
        };
        match Arc::try_unwrap(values) {
            Ok(mut values) => {
                if projection
                    .iter()
                    .enumerate()
                    .all(|(position, slot)| position == *slot)
                {
                    values.truncate(projection.len());
                    insert_values(columns, result, values);
                    return;
                }
                let mut remaining = vec![0usize; values.len()];
                for slot in projection.iter().copied().filter(|slot| *slot != NULL_SLOT) {
                    if let Some(count) = remaining.get_mut(slot) {
                        *count += 1;
                    }
                }
                for slot in projection.iter().copied() {
                    let value = if slot == NULL_SLOT {
                        Value::Null
                    } else {
                        let Some(count) = remaining.get_mut(slot) else {
                            insert_value(columns, result, Value::Null);
                            continue;
                        };
                        *count -= 1;
                        if *count == 0 {
                            values
                                .get_mut(slot)
                                .map(|value| std::mem::replace(value, Value::Null))
                                .unwrap_or(Value::Null)
                        } else {
                            values.get(slot).cloned().unwrap_or(Value::Null)
                        }
                    };
                    insert_value(columns, result, value);
                }
            }
            Err(values) => {
                for slot in projection.iter().copied() {
                    let value = if slot == NULL_SLOT {
                        Value::Null
                    } else {
                        values.get(slot).cloned().unwrap_or(Value::Null)
                    };
                    insert_value(columns, result, value);
                }
            }
        }
    }

    /// Consume this fragment at an explicit row-materialization boundary. Unshared contiguous values, and the common prefix projection emitted by blocking operators, retain their existing allocations instead of being cloned one value at a time.
    fn into_values(self) -> Vec<Value> {
        let (values, projection) = match self {
            Self::Constant { value, width } => return vec![value.clone(); width],
            Self::Stored { values, projection } => (values, projection),
        };
        let Some(projection) = projection else {
            return Arc::try_unwrap(values).unwrap_or_else(|values| values.as_ref().clone());
        };
        let mut values = match Arc::try_unwrap(values) {
            Ok(values) => values,
            Err(values) => {
                return projection
                    .iter()
                    .map(|slot| {
                        if *slot == NULL_SLOT {
                            Value::Null
                        } else {
                            values.get(*slot).cloned().unwrap_or(Value::Null)
                        }
                    })
                    .collect();
            }
        };
        if projection
            .iter()
            .enumerate()
            .all(|(position, slot)| position == *slot)
        {
            values.truncate(projection.len());
            return values;
        }

        let mut remaining = vec![0usize; values.len()];
        for slot in projection.iter().copied().filter(|slot| *slot != NULL_SLOT) {
            if let Some(count) = remaining.get_mut(slot) {
                *count += 1;
            }
        }
        let mut values = values.into_iter().map(Some).collect::<Vec<_>>();
        projection
            .iter()
            .map(|slot| {
                if *slot == NULL_SLOT {
                    return Value::Null;
                }
                let Some(count) = remaining.get_mut(*slot) else {
                    return Value::Null;
                };
                *count -= 1;
                if *count == 0 {
                    values[*slot].take().unwrap_or(Value::Null)
                } else {
                    values[*slot].clone().unwrap_or(Value::Null)
                }
            })
            .collect()
    }
}

fn insert_values<'a>(
    columns: &mut impl Iterator<Item = &'a String>,
    result: &mut ResultRow,
    values: impl IntoIterator<Item = Value>,
) {
    for value in values {
        insert_value(columns, result, value);
    }
}

fn insert_value<'a>(
    columns: &mut impl Iterator<Item = &'a String>,
    result: &mut ResultRow,
    value: Value,
) {
    if let Some(column) = columns.next() {
        result.insert(column.clone(), value);
    }
}

impl PhysicalRow {
    /// Consume an owned row at a positional state boundary, moving uniquely owned fragments and cloning only shared or multiply referenced values.
    pub fn into_physical_values(self) -> Vec<Value> {
        let mut fragments = self.into_value_fragments();
        if fragments.len() == 1 {
            return fragments.pop().unwrap_or_default();
        }
        let capacity = fragments.iter().map(Vec::len).sum();
        let mut values = Vec::with_capacity(capacity);
        for fragment in fragments {
            values.extend(fragment);
        }
        values
    }

    fn into_value_fragments(self) -> SmallVec<[Vec<Value>; INLINE_ROW_FRAGMENTS]> {
        self.fragments
            .into_iter()
            .map(RowFragment::into_values)
            .collect()
    }
}
