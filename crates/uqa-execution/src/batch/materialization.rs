//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Final conversion from positional physical rows to named result rows.

use super::{
    Arc, PhysicalRow, ResultRow, RowFragment, RowSchema, SmallVec, Value, INLINE_ROW_FRAGMENTS,
    NULL_SLOT,
};

impl RowSchema {
    pub(super) fn materialize_result_row(&self, row: PhysicalRow) -> ResultRow {
        if self.index.cold.identity_layout {
            return self.materialize_identity_result_row(row);
        }
        self.materialize_remapped_result_row(row)
    }

    pub(super) fn materialize_remapped_result_row(&self, row: PhysicalRow) -> ResultRow {
        let mut fragments = row.into_value_fragments();
        debug_assert_eq!(
            self.physical_width(),
            fragments.iter().map(Vec::len).sum::<usize>()
        );
        let last_use = self
            .index
            .cold
            .materialization_last_use
            .as_deref()
            .expect("remapped result schema has a materialization plan");
        let mut result = ResultRow::new();
        for (logical, column) in self.columns().iter().enumerate() {
            let slot = self.index.slots[logical];
            let value = if slot == NULL_SLOT {
                Value::Null
            } else {
                materialize_fragment_slot(&mut fragments, slot, last_use[slot] == logical)
            };
            result.insert(column.clone(), value);
        }
        result
    }

    pub(super) fn materialize_identity_result_row(&self, row: PhysicalRow) -> ResultRow {
        let fragments = row.into_value_fragments();
        debug_assert_eq!(self.len(), fragments.iter().map(Vec::len).sum::<usize>());
        let mut columns = self.columns().iter();
        let mut result = ResultRow::new();
        for value in fragments.into_iter().flatten() {
            let Some(column) = columns.next() else {
                break;
            };
            result.insert(column.clone(), value);
        }
        result
    }
}

fn materialize_fragment_slot(fragments: &mut [Vec<Value>], mut slot: usize, take: bool) -> Value {
    for fragment in fragments {
        if slot < fragment.len() {
            return if take {
                std::mem::replace(&mut fragment[slot], Value::Null)
            } else {
                fragment[slot].clone()
            };
        }
        slot -= fragment.len();
    }
    Value::Null
}

impl RowFragment {
    /// Consume this fragment at an explicit row-materialization boundary. Unshared contiguous values, and the common prefix projection emitted by blocking operators, retain their existing allocations instead of being cloned one value at a time.
    fn into_values(self) -> Vec<Value> {
        let Self { values, projection } = self;
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

impl PhysicalRow {
    fn into_value_fragments(self) -> SmallVec<[Vec<Value>; INLINE_ROW_FRAGMENTS]> {
        self.fragments
            .into_iter()
            .map(RowFragment::into_values)
            .collect()
    }
}
