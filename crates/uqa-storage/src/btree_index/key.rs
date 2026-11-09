//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted physical keys keep original payload ownership through cloned writable indexes.

use std::{borrow::Borrow, cmp::Ordering, sync::Arc};
use uqa_core::{memory::Budgeted, Value};

#[derive(Debug, Clone)]
pub(super) enum Key {
    Native(Value),
    Admitted(Arc<Budgeted<Value>>),
}

impl Key {
    pub(super) fn value(&self) -> &Value {
        match self {
            Self::Native(value) => value,
            Self::Admitted(value) => value,
        }
    }
}

impl Borrow<Value> for Key {
    fn borrow(&self) -> &Value {
        self.value()
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value()
    }
}
impl Eq for Key {}
impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value().cmp(other.value())
    }
}
