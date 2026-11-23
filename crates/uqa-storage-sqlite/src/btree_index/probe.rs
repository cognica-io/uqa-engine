//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded seeks over the physical lookup projection: one key for a value, one interval of keys for a range. Unsupported stored comparison domains decline before returning candidates.

use super::{decode_doc_id, equality, DocId, Result, Value};
use rusqlite::{
    params,
    types::{ToSqlOutput, ValueRef},
    Connection,
};
use std::ops::Bound;
use uqa_core::memory::BudgetedVec;
use uqa_storage::read_control::StorageReadControl;

#[cfg(test)]
thread_local! { pub(super) static PROBE_VM_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

/// The keys that a probe selects in one index: the key of a value, or the keys between the keys of two values.
pub(crate) struct IndexProbe {
    keys: Keys,
}

enum Keys {
    One(BudgetedVec<u8>),
    Between(Bound<BudgetedVec<u8>>, Bound<BudgetedVec<u8>>),
}

impl IndexProbe {
    pub(super) fn equal(value: &Value, control: &StorageReadControl) -> Result<Option<Self>> {
        Ok(equality::key(value, control)?.map(|key| Self {
            keys: Keys::One(key),
        }))
    }

    /// The values between two ends as Core orders them. `None` when an end has no key.
    pub(super) fn range(
        low: Bound<&Value>,
        high: Bound<&Value>,
        control: &StorageReadControl,
    ) -> Result<Option<Self>> {
        let key = |end: Bound<&Value>| -> Result<Option<Bound<BudgetedVec<u8>>>> {
            Ok(match end {
                Bound::Unbounded => Some(Bound::Unbounded),
                Bound::Included(value) => equality::key(value, control)?.map(Bound::Included),
                Bound::Excluded(value) => equality::key(value, control)?.map(Bound::Excluded),
            })
        };
        let (Some(low), Some(high)) = (key(low)?, key(high)?) else {
            return Ok(None);
        };
        Ok(Some(Self {
            keys: Keys::Between(low, high),
        }))
    }

    pub(crate) fn matches(
        &self,
        value: &Value,
        control: &StorageReadControl,
    ) -> Result<Option<bool>> {
        Ok(equality::key(value, control)?.map(|key| match &self.keys {
            Keys::One(selected) => *key == **selected,
            Keys::Between(low, high) => {
                let above = match low {
                    Bound::Unbounded => true,
                    Bound::Included(low) => *key >= **low,
                    Bound::Excluded(low) => *key > **low,
                };
                let below = match high {
                    Bound::Unbounded => true,
                    Bound::Included(high) => *key <= **high,
                    Bound::Excluded(high) => *key < **high,
                };
                above && below
            }
        }))
    }

    #[cfg(test)]
    pub(crate) fn read(
        &self,
        connection: &Connection,
        table: &str,
        field: ValueRef<'_>,
        control: &StorageReadControl,
    ) -> Result<Option<BudgetedVec<DocId>>> {
        let mut ids = BudgetedVec::new(control.memory());
        let supported = self.visit(connection, table, field, control, &mut |id| {
            ids.push(id)?;
            Ok(true)
        })?;
        Ok(supported.then_some(ids))
    }

    /// Visit the selected postings in document order while `visit` returns true. Returns false without visiting when the index cannot answer: it is incomplete, holds a value without a key, or the postings of an interval exceed the caller's memory.
    pub(crate) fn visit(
        &self,
        connection: &Connection,
        table: &str,
        field: ValueRef<'_>,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(DocId) -> Result<bool>,
    ) -> Result<bool> {
        if !super::coverage::complete(connection, table, field)?
            || !supported_domain(connection, table, field, control)?
        {
            return Ok(false);
        }
        match &self.keys {
            Keys::One(key) => {
                // The postings of one key follow in document order.
                let mut statement = connection.prepare_cached("SELECT doc_id FROM _btree_equality_keys WHERE table_name = ?1 AND field = ?2 AND equality_key = ?3 ORDER BY doc_id")?;
                control.check()?;
                let mut rows =
                    statement.query(params![table, ToSqlOutput::Borrowed(field), &**key])?;
                while let Some(row) = rows.next()? {
                    control.check()?;
                    if !visit(decode_doc_id(row.get(0)?)?)? {
                        break;
                    }
                }
                drop(rows);
                #[cfg(test)]
                PROBE_VM_STEPS.set(
                    PROBE_VM_STEPS.get()
                        + statement.reset_status(rusqlite::StatementStatus::VmStep) as usize,
                );
                Ok(true)
            }
            Keys::Between(low, high) => {
                let Some(ids) = interval(connection, table, field, low, high, control)? else {
                    return Ok(false);
                };
                for &id in ids.iter() {
                    control.check()?;
                    if !visit(id)? {
                        break;
                    }
                }
                Ok(true)
            }
        }
    }
}

/// The postings between two keys, which the index holds in key order, in document order. `None` when they exceed the caller's memory: the caller then answers without the index.
fn interval(
    connection: &Connection,
    table: &str,
    field: ValueRef<'_>,
    low: &Bound<BudgetedVec<u8>>,
    high: &Bound<BudgetedVec<u8>>,
    control: &StorageReadControl,
) -> Result<Option<BudgetedVec<DocId>>> {
    // An unbounded low end is the empty key, which no key precedes.
    let (low, above) = match low {
        Bound::Unbounded => (&[][..], ">="),
        Bound::Included(low) => (&**low, ">="),
        Bound::Excluded(low) => (&**low, ">"),
    };
    let (high, below) = match high {
        Bound::Unbounded => (None, ""),
        Bound::Included(high) => (Some(&**high), " AND equality_key <= ?4"),
        Bound::Excluded(high) => (Some(&**high), " AND equality_key < ?4"),
    };
    let mut statement = connection.prepare_cached(&format!("SELECT doc_id FROM _btree_equality_keys WHERE table_name = ?1 AND field = ?2 AND equality_key {above} ?3{below}"))?;
    control.check()?;
    let field = ToSqlOutput::Borrowed(field);
    let mut rows = match high {
        Some(high) => statement.query(params![table, field, low, high])?,
        None => statement.query(params![table, field, low])?,
    };
    let mut ids = BudgetedVec::new(control.memory());
    let mut fits = true;
    while let Some(row) = rows.next()? {
        control.check()?;
        if ids.push(decode_doc_id(row.get(0)?)?).is_err() {
            fits = false;
            break;
        }
    }
    drop(rows);
    #[cfg(test)]
    PROBE_VM_STEPS.set(
        PROBE_VM_STEPS.get() + statement.reset_status(rusqlite::StatementStatus::VmStep) as usize,
    );
    if !fits {
        return Ok(None);
    }
    ids.sort_unstable();
    Ok(Some(ids))
}

/// Finish the provider-owned workspace before handing the caller its ordinary result buffer.
pub(crate) fn into_ids(mut ids: BudgetedVec<DocId>) -> Vec<DocId> {
    ids.sort_unstable();
    let mut retained = 0;
    for index in 0..ids.len() {
        if retained == 0 || ids[index] != ids[retained - 1] {
            ids[retained] = ids[index];
            retained += 1;
        }
    }
    ids.truncate(retained);
    ids.into_parts().0
}

/// Unsupported keys are indexed as SQL NULL. This check is one equality seek, independent of the number of scalar or composite values.
fn supported_domain(
    connection: &Connection,
    table: &str,
    field: ValueRef<'_>,
    control: &StorageReadControl,
) -> Result<bool> {
    control.check()?;
    let mut statement = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM _btree_equality_keys WHERE table_name = ?1 AND field = ?2 AND equality_key IS NULL)")?;
    let found: bool = statement.query_row(params![table, ToSqlOutput::Borrowed(field)], |row| {
        row.get(0)
    })?;
    #[cfg(test)]
    PROBE_VM_STEPS.set(
        PROBE_VM_STEPS.get() + statement.reset_status(rusqlite::StatementStatus::VmStep) as usize,
    );
    Ok(!found)
}

#[cfg(test)]
mod tests;
