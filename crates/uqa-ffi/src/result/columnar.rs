//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained native column batches and shared, admitted cursor descriptors.

use std::{ops::Range, sync::Arc};
use uqa_core::{
    memory::{Budgeted, BudgetedVec, Produced},
    Value,
};
use uqa_engine::{ColumnarBatch, SQLCursor};
use uqa_sql::ColumnType;

use crate::{boundary, session::Session};

pub(crate) struct CursorColumn {
    pub(crate) name: Produced<String>,
    pub(crate) ty: Option<Produced<ColumnType>>,
}

pub(crate) type CursorColumns = Arc<Budgeted<BudgetedVec<CursorColumn>>>;

pub(crate) fn columns(native: &SQLCursor, session: &Session) -> boundary::Result<CursorColumns> {
    let control = session.control();
    let mut columns = BudgetedVec::new(&session.memory);
    columns.reserve(native.columns().len())?;
    for (index, name) in native.columns().iter().enumerate() {
        columns.push(CursorColumn {
            name: control.copy_text(name)?,
            ty: native
                .column_types()
                .get(index)
                .and_then(Option::as_ref)
                .map(|ty| ty.clone_with_control(&control))
                .transpose()?,
        })?;
    }
    Ok(Budgeted::new(columns, session.memory.empty_reservation()).into_shared()?)
}

pub(crate) struct CursorSlice {
    pub(crate) columns: CursorColumns,
    pub(crate) batch: Arc<Budgeted<ColumnarBatch>>,
    pub(crate) rows: Range<usize>,
}

impl CursorSlice {
    pub(crate) fn value_at(&self, row: usize, column: usize) -> Option<&Value> {
        if row >= self.rows.len() {
            return None;
        }
        self.batch
            .columns()
            .get(column)?
            .values
            .get(self.rows.start + row)
    }
}
