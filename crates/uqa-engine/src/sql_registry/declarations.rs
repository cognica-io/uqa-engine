//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable registration names for a snapshot that owns no application callbacks.

use crate::SQLError;
use uqa_core::memory::{Budgeted, MemoryBudget, RetainedMap, RetainedMapSnapshot};

pub(crate) use uqa_sql::binding::view_dependencies::ApplicationFunctionKind as RegisteredKind;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RegisteredKinds {
    pub(crate) scalar: bool,
    pub(crate) table: bool,
    pub(crate) aggregate: bool,
}

impl RegisteredKinds {
    pub(crate) fn contains(self, kind: RegisteredKind) -> bool {
        match kind {
            RegisteredKind::Scalar => self.scalar,
            RegisteredKind::Table => self.table,
            RegisteredKind::Aggregate => self.aggregate,
        }
    }
}

pub(crate) struct Declarations(RetainedMap<String, RegisteredKinds>);
pub(crate) type CapturedDeclarations = Budgeted<RetainedMapSnapshot<String, RegisteredKinds>>;

impl Default for Declarations {
    fn default() -> Self {
        Self(RetainedMap::new(&MemoryBudget::new(usize::MAX)))
    }
}

impl Declarations {
    pub(super) fn get(&self, name: &str) -> RegisteredKinds {
        self.0.get(name).copied().unwrap_or_default()
    }

    pub(super) fn set(
        &mut self,
        name: &str,
        kind: RegisteredKind,
        installed: bool,
    ) -> Result<(), SQLError> {
        let mut kinds = self.0.get(name).copied().unwrap_or_default();
        let selected = match kind {
            RegisteredKind::Scalar => &mut kinds.scalar,
            RegisteredKind::Table => &mut kinds.table,
            RegisteredKind::Aggregate => &mut kinds.aggregate,
        };
        if *selected == installed {
            return Ok(());
        }
        *selected = installed;
        if kinds == RegisteredKinds::default() {
            self.0.remove(name)?;
        } else {
            let name = name.to_owned();
            let capacity = name.capacity();
            self.0.insert(name, kinds, capacity)?;
        }
        Ok(())
    }

    pub(super) fn capture(&self, memory: &MemoryBudget) -> Result<CapturedDeclarations, SQLError> {
        Ok(self.0.capture(memory)?)
    }
}
