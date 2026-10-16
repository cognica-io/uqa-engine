//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed value traversal keeps its explicit stack in the selected allowance.

use super::{output::resource_error, BudgetedVec, ExecResult, Value};
use uqa_core::memory::MemoryBudget;

pub(super) enum Children<'a> {
    Values(std::slice::Iter<'a, Value>),
    Array(std::slice::Iter<'a, Value>, Option<u32>),
    Record(std::slice::Iter<'a, (String, Value)>),
    SQLRecord(uqa_sql::expr::SQLRecordFields<'a>, usize),
    Map(std::collections::btree_map::Iter<'a, String, Value>),
}

impl<'a> Children<'a> {
    pub(super) fn next(&mut self) -> Option<(Option<&'a str>, &'a Value, Option<u32>)> {
        match self {
            Self::Values(values) => values.next().map(|value| (None, value, None)),
            Self::Array(values, oid) => values.next().map(|value| (None, value, *oid)),
            Self::Record(fields) => fields.next().map(|(_, value)| (None, value, None)),
            Self::SQLRecord(fields, index) => {
                let value = fields.value(*index)?;
                let oid = fields.field_type_oid(*index);
                *index += 1;
                Some((None, value, oid))
            }
            Self::Map(fields) => fields
                .next()
                .map(|(name, value)| (Some(name.as_str()), value, None)),
        }
    }
}

pub(super) enum Frames<'a> {
    Owned(Vec<Children<'a>>),
    Budgeted(BudgetedVec<Children<'a>>),
}

impl<'a> Frames<'a> {
    pub(super) fn new(budget: Option<&MemoryBudget>) -> Self {
        match budget {
            Some(budget) => Self::Budgeted(BudgetedVec::new(budget)),
            None => Self::Owned(Vec::new()),
        }
    }

    pub(super) fn push(&mut self, value: Children<'a>) -> ExecResult<()> {
        match self {
            Self::Owned(values) => values.push(value),
            Self::Budgeted(values) => values.push(value).map_err(resource_error)?,
        }
        Ok(())
    }

    pub(super) fn last_mut(&mut self) -> Option<&mut Children<'a>> {
        match self {
            Self::Owned(values) => values.last_mut(),
            Self::Budgeted(values) => values.last_mut(),
        }
    }

    pub(super) fn pop(&mut self) {
        match self {
            Self::Owned(values) => {
                values.pop();
            }
            Self::Budgeted(values) => {
                values.pop();
            }
        }
    }
}
