//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lexical shadowing moves prior entries with their exact nodes and restores them without allocating.

use super::super::{
    production::{BindingMap, BindingNameSet},
    RowSchema, SchemaScope,
};
use crate::{plan::CtePlan, SQLError};
use uqa_core::memory::{MemoryReservation, Produced, ProductionControl, ProductionVec};

pub(in crate::binding) struct CteScopeEntry {
    name: Produced<String>,
    schemas: BindingMap<RowSchema>,
    non_returning: BindingNameSet,
}

pub(in crate::binding) struct CteScopeStack {
    entries: Vec<CteScopeEntry>,
    _memory: Option<MemoryReservation>,
}

impl CteScopeStack {
    pub(in crate::binding) fn new(
        capacity: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut entries = ProductionVec::new(*control);
        entries.reserve(capacity)?;
        let (entries, memory) = entries.finish()?.into_parts();
        Ok(Self {
            entries,
            _memory: memory,
        })
    }

    pub(in crate::binding) fn push(&mut self, entry: CteScopeEntry) {
        assert!(
            self.entries.len() < self.entries.capacity(),
            "CTE scope capacity was admitted before publication"
        );
        self.entries.push(entry);
    }
}

impl SchemaScope {
    pub(in crate::binding) fn install_cte_schema(
        &mut self,
        cte: &CtePlan,
        schema: Option<RowSchema>,
    ) -> Result<CteScopeEntry, SQLError> {
        let control = self.control();
        control.check()?;
        let name = control.copy_text(&cte.name)?;
        let mut schemas = BindingMap::new();
        if let Some(schema) = schema {
            schemas.insert_with_control(&cte.name, schema, &control)?;
        }
        let mut non_returning = BindingNameSet::new();
        if !cte.body.returns_rows() {
            non_returning.insert_with_control(&cte.name, &control)?;
        }
        control.check()?;
        let previous = CteScopeEntry {
            name,
            schemas: self.ctes.extract(&cte.name),
            non_returning: self.non_returning_ctes.extract(&cte.name),
        };
        self.ctes.append(schemas);
        self.non_returning_ctes.append(non_returning);
        Ok(previous)
    }

    pub(in crate::binding) fn set_cte_schema(
        &mut self,
        name: &str,
        schema: RowSchema,
    ) -> Result<(), SQLError> {
        let owner = self.control().capture();
        self.ctes
            .insert_with_control(name, schema, &owner.control())?;
        Ok(())
    }

    pub(super) fn restore_owned_cte_schemas(&mut self, mut previous: CteScopeStack) {
        while let Some(entry) = previous.entries.pop() {
            self.ctes.remove(&entry.name);
            self.non_returning_ctes.remove(&entry.name);
            self.ctes.append(entry.schemas);
            self.non_returning_ctes.append(entry.non_returning);
        }
    }
}
