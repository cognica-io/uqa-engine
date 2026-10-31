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

#[derive(Debug, Clone)]
pub(super) enum RowFragment {
    Stored {
        values: Arc<Vec<Value>>,
        /// Fragment-local output slot -> stored value slot; absent for contiguous values.
        projection: Option<Arc<[usize]>>,
    },
    /// NULL extension and Boolean presence have no width-sized value storage.
    Constant { value: &'static Value, width: usize },
}

impl PartialEq for RowFragment {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Stored {
                    values: left,
                    projection: lp,
                },
                Self::Stored {
                    values: right,
                    projection: rp,
                },
            ) => left == right && lp == rp,
            (
                Self::Constant {
                    value: left,
                    width: lw,
                },
                Self::Constant {
                    value: right,
                    width: rw,
                },
            ) => left == right && lw == rw,
            (Self::Constant { value, width }, Self::Stored { values, projection })
            | (Self::Stored { values, projection }, Self::Constant { value, width }) => {
                projection.is_none()
                    && values.len() == *width
                    && values.iter().all(|stored| stored == *value)
            }
        }
    }
}

impl RowFragment {
    fn contiguous(values: Arc<Vec<Value>>) -> Self {
        Self::Stored {
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
            Self::Stored {
                values,
                projection: Some(projection),
            }
        }
    }

    pub(super) fn len(&self) -> usize {
        #[cfg(test)]
        FRAGMENT_WIDTH_READS.with(|reads| reads.set(reads.get() + 1));
        match self {
            Self::Stored { values, projection } => projection
                .as_ref()
                .map_or(values.len(), |projection| projection.len()),
            Self::Constant { width, .. } => *width,
        }
    }

    pub(super) fn get(&self, slot: usize) -> Option<&Value> {
        match self {
            Self::Constant { value, width } => (slot < *width).then_some(*value),
            Self::Stored { values, projection } => match projection.as_ref() {
                Some(projection) => match projection.get(slot).copied()? {
                    NULL_SLOT => Some(&NULL_VALUE),
                    stored => values.get(stored),
                },
                None => values.get(slot),
            },
        }
    }

    pub(super) fn has_shared_values(&self) -> bool {
        matches!(self, Self::Stored { values, .. } if Arc::strong_count(values) > 1)
    }

    pub(super) fn can_copy_directly(&self) -> bool {
        matches!(self, Self::Constant { .. }) || self.has_shared_values()
    }

    fn stored_slot(&self, slot: usize) -> Option<usize> {
        match self {
            Self::Constant { width, .. } => (slot < *width).then_some(slot),
            Self::Stored { values, projection } => match projection.as_ref() {
                Some(projection) => projection.get(slot).copied(),
                None => (slot < values.len()).then_some(slot),
            },
        }
    }

    fn into_prefix(mut self, width: usize) -> Self {
        debug_assert!(width <= self.len());
        if width == self.len() {
            return self;
        }
        match &mut self {
            Self::Constant { width: old, .. } => *old = width,
            Self::Stored { values, projection } => {
                if let Some(slots) = projection.as_ref() {
                    *projection = Some(Arc::from(&slots[..width]));
                } else if let Some(values) = Arc::get_mut(values) {
                    values.truncate(width);
                } else {
                    *projection = Some((0..width).collect::<Arc<[usize]>>());
                }
            }
        }
        self
    }

    #[cfg(test)]
    pub(super) fn stored_values(&self) -> &Arc<Vec<Value>> {
        let Self::Stored { values, .. } = self else {
            panic!("expected stored fragment")
        };
        values
    }

    #[cfg(test)]
    pub(super) fn projection(&self) -> Option<&Arc<[usize]>> {
        match self {
            Self::Stored { projection, .. } => projection.as_ref(),
            Self::Constant { .. } => None,
        }
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
    #[cfg(test)]
    pub(crate) fn count_fragment_width_reads<T>(visit: impl FnOnce() -> T) -> (T, usize) {
        FRAGMENT_WIDTH_READS.with(|reads| reads.set(0));
        let output = visit();
        (output, FRAGMENT_WIDTH_READS.with(std::cell::Cell::get))
    }

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
        Self::constant(&NULL_VALUE, width)
    }

    pub(crate) fn present() -> Self {
        Self::constant(&Value::Bool(true), 1)
    }

    fn constant(value: &'static Value, width: usize) -> Self {
        let mut fragments = RowFragments::new();
        if width != 0 {
            fragments.push(RowFragment::Constant { value, width });
        }
        Self {
            fragments,
            lock_origins: None,
        }
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

    /// Visit physical slots once in order, including absent values in malformed projections so the caller can preserve its slot diagnostic.
    pub(crate) fn values(&self) -> impl Iterator<Item = Option<&Value>> {
        self.fragments
            .iter()
            .flat_map(|fragment| (0..fragment.len()).map(move |slot| fragment.get(slot)))
    }

    /// Re-express selected flattened slots as a compact positional row while
    /// sharing the underlying value vectors. Consecutive slots backed by the
    /// same source fragment share one projection fragment; no `Value` (and in
    /// particular no string payload) is cloned.
    pub(crate) fn project_slots(&self, slots: &[usize]) -> Self {
        let mut output = RowFragments::new();
        let null = RowFragment::Constant {
            value: &NULL_VALUE,
            width: 1,
        };
        let null_source = self.fragments.len();
        let mut current_source = None;
        let mut current = None;
        let mut current_width = 0;
        let mut current_projection = Vec::new();
        let flush = |output: &mut RowFragments,
                     fragment: &mut Option<&RowFragment>,
                     width: usize,
                     projection: &mut Vec<usize>| {
            if let Some(fragment) = fragment.take() {
                output.push(match fragment {
                    RowFragment::Constant { value, .. } => RowFragment::Constant { value, width },
                    RowFragment::Stored { values, .. } => RowFragment::projected(
                        Arc::clone(values),
                        Arc::from(std::mem::take(projection)),
                    ),
                });
            }
        };
        for requested in slots {
            let mut remaining = *requested;
            let mut resolved = None;
            if remaining != NULL_SLOT {
                for (index, fragment) in self.fragments.iter().enumerate() {
                    if remaining < fragment.len() {
                        resolved = fragment
                            .stored_slot(remaining)
                            .filter(|slot| *slot != NULL_SLOT)
                            .map(|slot| (index, fragment, slot));
                        break;
                    }
                    remaining -= fragment.len();
                }
            }
            let (source, fragment, stored) = resolved.unwrap_or((null_source, &null, NULL_SLOT));
            if current_source != Some(source) {
                flush(
                    &mut output,
                    &mut current,
                    current_width,
                    &mut current_projection,
                );
                current_source = Some(source);
                current = Some(fragment);
                current_width = 0;
            }
            current_width += 1;
            if matches!(fragment, RowFragment::Stored { .. }) {
                current_projection.push(stored);
            }
        }
        flush(
            &mut output,
            &mut current,
            current_width,
            &mut current_projection,
        );
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

#[cfg(test)]
thread_local! {
    static FRAGMENT_WIDTH_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
