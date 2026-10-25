//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execution ownership of SQL-invisible, repeatable row sources.

use std::cell::RefCell;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;

use uqa_sql::ast::{InternalRelationId, RetainedRowSource};
use uqa_sql::SQLError;

use crate::{SharedSpill, SharedSpillScan};

#[derive(Clone)]
struct RetainedRows {
    rows: SharedSpill,
}

#[cfg(test)]
mod tests;

thread_local! {
    static ACTIVE_ROWS: RefCell<Arc<BTreeMap<InternalRelationId, RetainedRows>>> = RefCell::default();
}

/// Immutable owners captured for a statement's workers. Capturing and entering a context share the spill owners without copying rows or their metadata maps.
#[derive(Clone, Default)]
pub struct RetainedRowContext(Arc<BTreeMap<InternalRelationId, RetainedRows>>);

impl RetainedRowContext {
    #[must_use]
    pub fn current() -> Self {
        ACTIVE_ROWS.with(|active| Self(active.borrow().clone()))
    }

    pub(crate) fn enter(&self) -> RetainedRowThreadScope {
        RetainedRowThreadScope {
            previous: ACTIVE_ROWS.with(|active| active.replace(self.0.clone())),
            _thread: PhantomData,
        }
    }
}

pub(crate) struct RetainedRowThreadScope {
    previous: Arc<BTreeMap<InternalRelationId, RetainedRows>>,
    _thread: PhantomData<Rc<()>>,
}

impl Drop for RetainedRowThreadScope {
    fn drop(&mut self) {
        ACTIVE_ROWS.with(|active| {
            std::mem::swap(&mut *active.borrow_mut(), &mut self.previous);
        });
    }
}

/// Own one source until its synchronous action finishes. Nested actions have independent opaque identities; an error or unwind removes exactly this source. The guard cannot move to another thread, while an opened scan owns its independent spill reader.
pub struct RetainedRowScope {
    descriptor: RetainedRowSource,
    _thread: PhantomData<Rc<()>>,
}

impl RetainedRowScope {
    pub fn enter(relation: InternalRelationId, rows: SharedSpill) -> Result<Self, SQLError> {
        let descriptor = RetainedRowSource::new(relation, rows.row_schema().clone(), rows.rows())?;
        ACTIVE_ROWS.with(|active| {
            match Arc::make_mut(&mut active.borrow_mut()).entry(relation) {
                Entry::Occupied(_) => Err(SQLError::Internal(
                    "retained row source is already active".into(),
                )),
                Entry::Vacant(entry) => {
                    entry.insert(RetainedRows { rows });
                    Ok(())
                }
            }
        })?;
        Ok(Self {
            descriptor,
            _thread: PhantomData,
        })
    }

    pub fn descriptor(&self) -> &RetainedRowSource {
        &self.descriptor
    }
}

impl Drop for RetainedRowScope {
    fn drop(&mut self) {
        ACTIVE_ROWS.with(|active| {
            let removed =
                Arc::make_mut(&mut active.borrow_mut()).remove(&self.descriptor.relation());
            debug_assert!(removed.is_some(), "retained row source ownership was lost");
        });
    }
}

fn retained_rows(source: &RetainedRowSource) -> Result<RetainedRows, SQLError> {
    ACTIVE_ROWS.with(|active| {
        let active = active.borrow();
        let rows = active.get(&source.relation()).ok_or_else(|| {
            SQLError::Internal("retained row source is outside its execution scope".into())
        })?;
        if rows.rows.rows() != source.rows() || rows.rows.row_schema() != source.schema() {
            return Err(SQLError::Internal(
                "retained row source does not match its declared rows and schema".into(),
            ));
        }
        Ok(rows.clone())
    })
}

#[cfg(test)]
pub(crate) fn build_retained_source(
    source: &RetainedRowSource,
) -> Result<SharedSpillScan, SQLError> {
    Ok(SharedSpillScan::new(retained_rows(source)?.rows))
}

pub(super) fn build_retained_source_operator(
    source: &RetainedRowSource,
) -> Result<SharedSpillScan, SQLError> {
    Ok(SharedSpillScan::new(retained_rows(source)?.rows))
}
