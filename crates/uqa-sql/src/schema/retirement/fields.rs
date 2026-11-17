//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Owning collection cursors transfer one field into the existing SQL field/type retirement state.

use crate::{
    ast::InternalColumnRef,
    ir::ownership::Retirement,
    plan::retirement::Retire,
    schema::{ColumnIdentity, ScoreSource},
};
use uqa_core::memory::{MemoryReservation, OwnedMapIntoIter};

pub(in crate::schema) trait Input {
    type Retired: Retire;
    fn into_retirement(self) -> Self::Retired;
}

impl<T: Retire> Input for T {
    type Retired = Self;
    fn into_retirement(self) -> Self {
        self
    }
}

pub(in crate::schema) struct Sequence<I: Iterator>
where
    I::Item: Input,
{
    source: I,
    current: Option<<I::Item as Input>::Retired>,
}

impl<I: Iterator> Sequence<I>
where
    I::Item: Input,
{
    pub(in crate::schema) fn new(source: I) -> Self {
        Self {
            source,
            current: None,
        }
    }
}

impl<I: Iterator> Retire for Sequence<I>
where
    I::Item: Input,
{
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        if let Some(current) = &mut self.current {
            if current.retire_step(positions, scalar) {
                self.current = None;
            }
            return false;
        }
        match self.source.next() {
            Some(value) => {
                self.current = Some(value.into_retirement());
                false
            }
            None => true,
        }
    }
}

impl<I: Iterator> Drop for Sequence<I>
where
    I::Item: Input,
{
    fn drop(&mut self) {
        self.retire();
    }
}

pub(in crate::schema) struct Entry<K, V> {
    pub key: K,
    pub value: V,
    pub memory: (Option<MemoryReservation>, Option<MemoryReservation>),
}

pub(in crate::schema) struct RetiredEntry<K, V> {
    key: K,
    value: V,
    _memory: (Option<MemoryReservation>, Option<MemoryReservation>),
}

impl<K: Input, V: Input> Input for Option<Entry<K, V>> {
    type Retired = Option<RetiredEntry<K::Retired, V::Retired>>;
    fn into_retirement(self) -> Self::Retired {
        self.map(|entry| RetiredEntry {
            key: entry.key.into_retirement(),
            value: entry.value.into_retirement(),
            _memory: entry.memory,
        })
    }
}

impl<K: Retire, V: Retire> Retire for RetiredEntry<K, V> {
    fn retire_step(&mut self, positions: &mut [usize], scalar: &mut Option<Retirement>) -> bool {
        self.key.retire_step(positions, scalar) && self.value.retire_step(positions, scalar)
    }
}

pub(in crate::schema) struct OrderedSlots<K, V>(pub OwnedMapIntoIter<K, V>);

impl<K, V> Iterator for OrderedSlots<K, V> {
    type Item = Option<Entry<K, V>>;
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next_retained().map(|(key, value)| {
            let (key, memory) = key.into_parts();
            Some(Entry {
                key,
                value,
                memory: (memory, None),
            })
        })
    }
}

// These leaves have a fixed number of allocations and no owned descendants. The enclosing vector or entry releases them at its existing retirement boundary.
macro_rules! leaf {
    ($($ty:ty),* $(,)?) => {$(
        impl Retire for $ty {
            fn retire_step(&mut self, _: &mut [usize], _: &mut Option<Retirement>) -> bool {
                true
            }
        }
    )*};
}
leaf!(
    (),
    bool,
    usize,
    u32,
    InternalColumnRef,
    Box<str>,
    ColumnIdentity,
    ScoreSource
);
