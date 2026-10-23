//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move selected physical values into positional result owners.

use super::{Arc, PhysicalRow, RowFragment, RowSchema, SmallVec, Value, NULL_SLOT};

/// Output positions are retained independently of labels and visited in physical
/// slot order. The consumer initializes absent positions with SQL NULL.
pub(crate) struct SelectedRowMaterializer {
    slots: Box<[(usize, usize)]>,
    outputs: Box<[usize]>,
}

impl SelectedRowMaterializer {
    pub(crate) fn new(schema: &RowSchema, positions: &[Option<usize>]) -> Self {
        let outputs = positions
            .iter()
            .map(|position| {
                position
                    .and_then(|position| schema.layout_slots().get(position).copied())
                    .unwrap_or(NULL_SLOT)
            })
            .collect::<Box<[_]>>();
        let mut slots = outputs
            .iter()
            .enumerate()
            .filter_map(|(output, &slot)| (slot != NULL_SLOT).then_some((slot, output)))
            .collect::<Vec<_>>();
        slots.sort_unstable();
        Self {
            slots: slots.into_boxed_slice(),
            outputs,
        }
    }

    /// A single externally shared fragment cannot donate its values. Read its
    /// existing projection directly, without sorting aliases or attempting an
    /// atomic ownership transfer for every scan row.
    pub(crate) fn copy_shared_row(
        &self,
        row: &mut PhysicalRow,
        mut emit: impl FnMut(usize, Value),
    ) -> bool {
        let [fragment] = row.fragments.as_slice() else {
            return false;
        };
        if Arc::strong_count(&fragment.values) == 1 {
            return false;
        }
        for (output, &slot) in self.outputs.iter().enumerate() {
            emit(output, fragment.get(slot).cloned().unwrap_or(Value::Null));
        }
        // Release this row now: a later row may be the last owner and can then
        // donate its payload. Keep the inline fragment array in place.
        row.fragments.clear();
        row.lock_origins = None;
        true
    }

    #[cfg(test)]
    pub(crate) fn ownership_attempts() -> usize {
        OWNERSHIP_ATTEMPTS.with(std::cell::Cell::get)
    }

    pub(crate) fn materialize(&self, row: PhysicalRow, mut emit: impl FnMut(usize, Value)) {
        if row.fragments.len() > 1
            && row
                .fragments
                .iter()
                .any(|fragment| Arc::strong_count(&fragment.values) > 1)
        {
            self.materialize_shared_fragments(row, &mut emit);
            return;
        }
        let mut pending = self.slots.as_ref();
        let mut offset = 0;
        for fragment in row.fragments {
            let end = offset + fragment.len();
            let count = pending.partition_point(|(slot, _)| *slot < end);
            let (selected, rest) = pending.split_at(count);
            fragment.materialize_selected(selected, offset, &mut emit);
            pending = rest;
            offset = end;
        }
    }

    fn materialize_shared_fragments(&self, row: PhysicalRow, emit: &mut impl FnMut(usize, Value)) {
        let mut pending = self.slots.as_ref();
        let mut offset = 0;
        let mut groups = SmallVec::<[SelectedFragment; 2]>::new();
        for fragment in row.fragments {
            let end = offset + fragment.len();
            let count = pending.partition_point(|(slot, _)| *slot < end);
            let (selected, rest) = pending.split_at(count);
            let selected = selected
                .iter()
                .filter_map(|(slot, output)| {
                    let local = slot - offset;
                    let stored = match &fragment.projection {
                        Some(projection) => *projection.get(local)?,
                        None => local,
                    };
                    (stored != NULL_SLOT).then_some((stored, *output))
                })
                .collect::<SmallVec<[(usize, usize); 16]>>();
            if !selected.is_empty() {
                groups.push(SelectedFragment {
                    values: fragment.values,
                    selected,
                });
            }
            pending = rest;
            offset = end;
        }
        // Discard unselected aliases and merge selected aliases before testing
        // exclusivity. Row-local Arc handles do not require payload copies.
        groups.sort_unstable_by_key(|group| Arc::as_ptr(&group.values));
        while let Some(mut group) = groups.pop() {
            while groups
                .last()
                .is_some_and(|other| Arc::ptr_eq(&group.values, &other.values))
            {
                let other = groups.pop().expect("matching fragment exists");
                group.selected.extend(other.selected);
            }
            group.selected.sort_unstable();
            emit_selected_values(group.values, group.selected, emit);
        }
    }
}

struct SelectedFragment {
    values: Arc<Vec<Value>>,
    selected: SmallVec<[(usize, usize); 16]>,
}

impl RowFragment {
    fn materialize_selected(
        self,
        selected: &[(usize, usize)],
        offset: usize,
        emit: &mut impl FnMut(usize, Value),
    ) {
        if selected.is_empty() {
            return;
        }
        let Self { values, projection } = self;
        let Some(projection) = projection else {
            emit_selected_values(
                values,
                selected
                    .iter()
                    .map(|(slot, output)| (slot - offset, *output)),
                emit,
            );
            return;
        };
        // Resolve the fragment projection before counting repeated values. Two
        // different physical slots can refer to the same stored value.
        let mut selected = selected
            .iter()
            .filter_map(|(slot, output)| {
                let stored = *projection.get(slot - offset)?;
                (stored != NULL_SLOT).then_some((stored, *output))
            })
            .collect::<SmallVec<[(usize, usize); 16]>>();
        selected.sort_unstable();
        emit_selected_values(values, selected, emit);
    }
}

fn emit_selected_values(
    values: Arc<Vec<Value>>,
    selected: impl IntoIterator<Item = (usize, usize)>,
    emit: &mut impl FnMut(usize, Value),
) {
    #[cfg(test)]
    OWNERSHIP_ATTEMPTS.with(|count| count.set(count.get() + 1));
    let mut selected = selected.into_iter().peekable();
    match Arc::try_unwrap(values) {
        Ok(mut values) => {
            while let Some((slot, output)) = selected.next() {
                let Some(value) = values.get_mut(slot) else {
                    continue;
                };
                let repeated = selected.peek().is_some_and(|(next, _)| *next == slot);
                emit(
                    output,
                    if repeated {
                        value.clone()
                    } else {
                        std::mem::replace(value, Value::Null)
                    },
                );
            }
        }
        Err(values) => {
            for (slot, output) in selected {
                if let Some(value) = values.get(slot) {
                    emit(output, value.clone());
                }
            }
        }
    }
}

#[cfg(test)]
thread_local! {
    static OWNERSHIP_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
