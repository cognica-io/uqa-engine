//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Allocation-light physical row fragments and projections.

use super::{
    concat_lock_origins, Arc, ResultRow, RowLockOrigin, RowSchema, SmallVec, Value,
    INLINE_ROW_FRAGMENTS, NULL_SLOT, NULL_VALUE,
};

#[derive(Debug, Clone, PartialEq)]
pub(super) struct RowFragment {
    pub(super) values: Arc<Vec<Value>>,
    /// Fragment-local output slot -> stored value slot. `None` is the common
    /// contiguous case. A projection lets an in-memory scan share its stored
    /// row even when column pruning selects or reorders fields.
    pub(super) projection: Option<Arc<[usize]>>,
}

impl RowFragment {
    fn contiguous(values: Arc<Vec<Value>>) -> Self {
        Self {
            values,
            projection: None,
        }
    }

    fn projected(values: Arc<Vec<Value>>, projection: Arc<[usize]>) -> Self {
        debug_assert!(projection
            .iter()
            .all(|slot| *slot == NULL_SLOT || *slot < values.len()));
        let identity = projection.len() == values.len()
            && projection
                .iter()
                .enumerate()
                .all(|(index, slot)| index == *slot);
        if identity {
            Self::contiguous(values)
        } else {
            Self {
                values,
                projection: Some(projection),
            }
        }
    }

    pub(super) fn len(&self) -> usize {
        self.projection
            .as_ref()
            .map_or(self.values.len(), |projection| projection.len())
    }

    pub(super) fn get(&self, slot: usize) -> Option<&Value> {
        match self.projection.as_ref() {
            Some(projection) => match projection.get(slot).copied()? {
                NULL_SLOT => Some(&NULL_VALUE),
                stored => self.values.get(stored),
            },
            None => self.values.get(slot),
        }
    }

    fn stored_slot(&self, slot: usize) -> Option<usize> {
        match self.projection.as_ref() {
            Some(projection) => projection.get(slot).copied(),
            None => (slot < self.values.len()).then_some(slot),
        }
    }

    fn into_prefix(mut self, width: usize) -> Self {
        debug_assert!(width <= self.len());
        if width == self.len() {
            return self;
        }
        if let Some(projection) = self.projection.as_ref() {
            self.projection = Some(Arc::from(&projection[..width]));
            return self;
        }
        if let Some(values) = Arc::get_mut(&mut self.values) {
            values.truncate(width);
        } else {
            self.projection = Some((0..width).collect::<Arc<[usize]>>());
        }
        self
    }
}

pub(super) type RowFragments = SmallVec<[RowFragment; INLINE_ROW_FRAGMENTS]>;

/// A physical row owns no column names. Each fragment is created by a scan or
/// projection and shared thereafter; joining rows copies only `Arc` handles.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PhysicalRow {
    pub(super) fragments: RowFragments,
    pub(super) lock_origins: Option<Arc<Vec<RowLockOrigin>>>,
}

/// One output position in a mixed physical projection.
#[derive(Debug, Clone, PartialEq)]
pub enum RowProjectionValue {
    /// Reuse one flattened slot from the input row.
    InputSlot(usize),
    /// Append a newly computed value.
    Owned(Value),
}

impl PhysicalRow {
    pub fn from_values(values: Vec<Value>) -> Self {
        let mut fragments = RowFragments::new();
        if !values.is_empty() {
            fragments.push(RowFragment::contiguous(Arc::new(values)));
        }
        Self {
            fragments,
            lock_origins: None,
        }
    }

    /// Build a row by sharing a stored positional value vector and applying a
    /// fragment-local slot projection. Neither the values nor contained
    /// strings are cloned.
    pub fn from_shared_values(values: Arc<Vec<Value>>, projection: Arc<[usize]>) -> Self {
        let mut fragments = RowFragments::new();
        if !projection.is_empty() {
            fragments.push(RowFragment::projected(values, projection));
        }
        Self {
            fragments,
            lock_origins: None,
        }
    }

    pub fn from_result_row(schema: &RowSchema, mut row: ResultRow) -> Self {
        let values = schema
            .columns()
            .iter()
            .map(|column| row.remove(column).unwrap_or(Value::Null))
            .collect();
        Self::from_values(values)
    }

    pub fn nulls(width: usize) -> Self {
        Self::from_values(vec![Value::Null; width])
    }

    pub fn append_values(mut self, values: Vec<Value>) -> Self {
        if !values.is_empty() {
            self.fragments
                .push(RowFragment::contiguous(Arc::new(values)));
        }
        self
    }

    pub fn concat(left: &Self, right: &Self) -> Self {
        let mut fragments =
            RowFragments::with_capacity(left.fragments.len() + right.fragments.len());
        fragments.extend(left.fragments.iter().cloned());
        fragments.extend(right.fragments.iter().cloned());
        let lock_origins =
            concat_lock_origins(left.lock_origins.as_ref(), right.lock_origins.as_ref());
        Self {
            fragments,
            lock_origins,
        }
    }

    pub fn concat_left_owned(mut left: Self, right: &Self) -> Self {
        left.fragments.extend(right.fragments.iter().cloned());
        left.lock_origins =
            concat_lock_origins(left.lock_origins.as_ref(), right.lock_origins.as_ref());
        left
    }

    pub fn concat_right_owned(left: &Self, mut right: Self) -> Self {
        let mut fragments =
            RowFragments::with_capacity(left.fragments.len() + right.fragments.len());
        fragments.extend(left.fragments.iter().cloned());
        fragments.append(&mut right.fragments);
        let lock_origins =
            concat_lock_origins(left.lock_origins.as_ref(), right.lock_origins.as_ref());
        Self {
            fragments,
            lock_origins,
        }
    }

    pub(crate) fn value(&self, mut slot: usize) -> Option<&Value> {
        for fragment in &self.fragments {
            if slot < fragment.len() {
                return fragment.get(slot);
            }
            slot -= fragment.len();
        }
        None
    }

    /// Re-express selected flattened slots as a compact positional row while
    /// sharing the underlying value vectors. Consecutive slots backed by the
    /// same source fragment share one projection fragment; no `Value` (and in
    /// particular no string payload) is cloned.
    pub(crate) fn project_slots(&self, slots: &[usize]) -> Self {
        let mut output = RowFragments::new();
        let null_values = Arc::new(Vec::new());
        let null_source = self.fragments.len();
        let mut current_source = None;
        let mut current_values: Option<Arc<Vec<Value>>> = None;
        let mut current_projection = Vec::new();

        let flush = |output: &mut RowFragments,
                     values: &mut Option<Arc<Vec<Value>>>,
                     projection: &mut Vec<usize>| {
            if let Some(values) = values.take() {
                output.push(RowFragment::projected(
                    values,
                    Arc::from(std::mem::take(projection)),
                ));
            }
        };

        for requested in slots {
            let mut remaining = *requested;
            let resolved = if remaining == NULL_SLOT {
                None
            } else {
                let mut found = None;
                for (fragment_index, fragment) in self.fragments.iter().enumerate() {
                    if remaining < fragment.len() {
                        found = fragment
                            .stored_slot(remaining)
                            .filter(|slot| *slot != NULL_SLOT)
                            .map(|stored| (fragment_index, Arc::clone(&fragment.values), stored));
                        break;
                    }
                    remaining -= fragment.len();
                }
                found
            };
            let (source, values, stored) = resolved.map_or_else(
                || (null_source, Arc::clone(&null_values), NULL_SLOT),
                |(source, values, stored)| (source, values, stored),
            );
            if current_source != Some(source) {
                flush(&mut output, &mut current_values, &mut current_projection);
                current_source = Some(source);
                current_values = Some(values);
            }
            current_projection.push(stored);
        }
        flush(&mut output, &mut current_values, &mut current_projection);
        Self {
            fragments: output,
            lock_origins: self.lock_origins.clone(),
        }
    }

    /// Build an output row from shared input slots and newly computed values while preserving their requested order and sharing row metadata.
    pub fn project_with_values(
        &self,
        values: impl IntoIterator<Item = RowProjectionValue>,
    ) -> Self {
        fn flush_slots(source: &PhysicalRow, output: &mut RowFragments, slots: &mut Vec<usize>) {
            if slots.is_empty() {
                return;
            }
            let mut projected = source.project_slots(slots);
            output.append(&mut projected.fragments);
            slots.clear();
        }

        fn flush_owned(output: &mut RowFragments, owned: &mut Vec<Value>) {
            if owned.is_empty() {
                return;
            }
            output.push(RowFragment::contiguous(Arc::new(std::mem::take(owned))));
        }

        let mut fragments = RowFragments::new();
        let mut slots = Vec::new();
        let mut owned = Vec::new();
        for value in values {
            match value {
                RowProjectionValue::InputSlot(slot) => {
                    flush_owned(&mut fragments, &mut owned);
                    slots.push(slot);
                }
                RowProjectionValue::Owned(value) => {
                    flush_slots(self, &mut fragments, &mut slots);
                    owned.push(value);
                }
            }
        }
        flush_slots(self, &mut fragments, &mut slots);
        flush_owned(&mut fragments, &mut owned);
        Self {
            fragments,
            lock_origins: self.lock_origins.clone(),
        }
    }

    pub fn fragment_count(&self) -> usize {
        self.fragments.len()
    }

    pub(crate) fn into_prefix(self, width: usize) -> Self {
        let mut remaining = width;
        let mut fragments = RowFragments::new();
        for fragment in self.fragments {
            if remaining == 0 {
                break;
            }
            let fragment_width = fragment.len();
            if fragment_width <= remaining {
                fragments.push(fragment);
                remaining -= fragment_width;
            } else {
                fragments.push(fragment.into_prefix(remaining));
                remaining = 0;
            }
        }
        debug_assert_eq!(remaining, 0, "physical row prefix exceeds row width");
        Self {
            fragments,
            lock_origins: self.lock_origins,
        }
    }
}
