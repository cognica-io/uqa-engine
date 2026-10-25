//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed diagnostic properties retain construction leases until the final renderer releases them.

use crate::SQLError;
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

#[cfg(test)]
mod tests;

/// Native optimizer estimates, independent of measurements from an invocation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExplainPlanEstimate {
    pub startup: f64,
    pub total: f64,
    pub rows: f64,
    pub width: u64,
}

/// Actual `DataRow` payload construction, excluding transport headers and network I/O.
#[derive(Debug, Clone, Copy, Default)]
pub struct ExplainSerializationMetrics {
    pub bytes: u64,
    pub elapsed: std::time::Duration,
}

/// Physical diagnostic values keep numeric primitives inline and retain only admitted containers and copied text. Field names and fixed labels borrow static definitions.
#[derive(Debug)]
pub enum ExplainValue {
    Null,
    Bool(bool),
    Unsigned(u64),
    Float(f64),
    Label(&'static str),
    Text(Produced<String>),
    Object(Produced<Vec<(&'static str, Self)>>),
}

pub struct ExplainObject<'a> {
    fields: ProductionVec<'a, (&'static str, ExplainValue)>,
    control: ProductionControl<'a>,
}

impl<'a> ExplainObject<'a> {
    pub fn new(control: ProductionControl<'a>) -> Self {
        Self {
            fields: ProductionVec::new(control),
            control,
        }
    }

    pub fn insert(&mut self, key: &'static str, value: ExplainValue) -> Result<(), SQLError> {
        self.fields.push_produced(
            self.control
                .finish((key, value), self.control.empty_reservation())?,
        )?;
        Ok(())
    }

    pub fn text(&mut self, key: &'static str, value: &str) -> Result<(), SQLError> {
        self.insert(key, ExplainValue::Text(self.control.copy_text(value)?))
    }

    pub fn label(&mut self, key: &'static str, value: &'static str) -> Result<(), SQLError> {
        self.insert(key, ExplainValue::Label(value))
    }

    pub fn number(&mut self, key: &'static str, value: u64) -> Result<(), SQLError> {
        self.insert(key, ExplainValue::Unsigned(value))
    }

    pub fn optional_number(
        &mut self,
        key: &'static str,
        value: Option<u64>,
    ) -> Result<(), SQLError> {
        self.insert(
            key,
            value.map_or(ExplainValue::Null, ExplainValue::Unsigned),
        )
    }

    pub fn float(&mut self, key: &'static str, value: f64) -> Result<(), SQLError> {
        self.insert(key, ExplainValue::Float(value))
    }

    pub fn boolean(&mut self, key: &'static str, value: bool) -> Result<(), SQLError> {
        self.insert(key, ExplainValue::Bool(value))
    }

    pub fn null(&mut self, key: &'static str) -> Result<(), SQLError> {
        self.insert(key, ExplainValue::Null)
    }

    pub fn finish(self) -> Result<ExplainValue, SQLError> {
        let (mut fields, memory) = self.fields.finish()?.into_parts();
        fields.sort_unstable_by_key(|(key, _)| *key);
        Ok(ExplainValue::Object(self.control.finish(fields, memory)?))
    }
}
