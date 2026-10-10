//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded equality seeks over the physical equality projection. Unsupported stored comparison domains decline before returning candidates.

use super::{decode_doc_id, equality, DocId, Result, Value};
use rusqlite::{
    params,
    types::{ToSqlOutput, ValueRef},
    Connection,
};
use uqa_core::memory::BudgetedVec;
use uqa_storage::read_control::StorageReadControl;

#[cfg(test)]
thread_local! { pub(super) static PROBE_VM_STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub(crate) struct EqualityProbe {
    key: BudgetedVec<u8>,
}

impl EqualityProbe {
    pub(super) fn new(value: &Value, control: &StorageReadControl) -> Result<Option<Self>> {
        Ok(equality::key(value, control)?.map(|key| Self { key }))
    }

    pub(crate) fn matches(
        &self,
        value: &Value,
        control: &StorageReadControl,
    ) -> Result<Option<bool>> {
        Ok(equality::key(value, control)?.map(|key| *key == *self.key))
    }

    pub(crate) fn read(
        &self,
        connection: &Connection,
        table: &str,
        field: ValueRef<'_>,
        control: &StorageReadControl,
    ) -> Result<Option<BudgetedVec<DocId>>> {
        if !super::coverage::complete(connection, table, field)?
            || !supported_domain(connection, table, field, control)?
        {
            return Ok(None);
        }
        let mut ids = BudgetedVec::new(control.memory());
        let mut statement = connection.prepare_cached("SELECT doc_id FROM _btree_index_entries WHERE table_name = ?1 AND field = ?2 AND __uqa_btree_equal_v1(value_json) = ?3 ORDER BY doc_id")?;
        control.check()?;
        let mut rows = statement.query(params![table, ToSqlOutput::Borrowed(field), &*self.key])?;
        while let Some(row) = rows.next()? {
            control.check()?;
            ids.push(decode_doc_id(row.get(0)?)?)?;
        }
        drop(rows);
        #[cfg(test)]
        PROBE_VM_STEPS.set(
            PROBE_VM_STEPS.get()
                + statement.reset_status(rusqlite::StatementStatus::VmStep) as usize,
        );
        Ok(Some(ids))
    }
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
    let mut statement = connection.prepare_cached("SELECT EXISTS(SELECT 1 FROM _btree_index_entries WHERE table_name = ?1 AND field = ?2 AND __uqa_btree_equal_v1(value_json) IS NULL)")?;
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
