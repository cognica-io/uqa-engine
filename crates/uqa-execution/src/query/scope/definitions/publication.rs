//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete lexical map admission before publishing a multi-field binding.

use uqa_core::memory::ProductionControl;
use uqa_sql::{binding::production::BindingMap, SQLError};

pub(super) enum PendingEntry<V> {
    Replace(V),
    Insert(BindingMap<V>),
}

impl<V> PendingEntry<V> {
    pub(super) fn prepare(
        target: &BindingMap<V>,
        name: &str,
        value: V,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        if target.contains_key(name) {
            Ok(Self::Replace(value))
        } else {
            let mut entry = BindingMap::new();
            entry.insert_with_control(name, value, control)?;
            Ok(Self::Insert(entry))
        }
    }

    pub(super) fn publish(self, target: &mut BindingMap<V>, name: &str) {
        match self {
            Self::Replace(value) => {
                *target.get_mut(name).expect("prepared lexical replacement") = value;
            }
            Self::Insert(entry) => target.append(entry),
        }
    }
}
