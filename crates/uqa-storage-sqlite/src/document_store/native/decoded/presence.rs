//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Presence-only projections read bounded metadata pages before reentrant visitors.

use super::{BudgetedVec, DocId, Family, NativeDocumentRead, SQLiteResult, Value};

#[cfg(test)]
mod tests;

impl NativeDocumentRead<'_> {
    pub(super) fn visit_presence(
        &self,
        ids: &[DocId],
        visitor: &mut dyn FnMut(DocId, bool, &[&Value]) -> bool,
    ) -> SQLiteResult<()> {
        for page in ids.chunks(256) {
            self.snapshot.control.check()?;
            self.control.check()?;
            let presence = self.dense_presence(page)?;
            for (position, id) in page.iter().copied().enumerate() {
                self.snapshot.control.check()?;
                self.control.check()?;
                let present = match presence.as_ref() {
                    Some(flags) => flags[position] != 0,
                    None => self.contains(id)?,
                };
                // The fixed view and metadata page remain alive after the physical cursor closes.
                let more = visitor(id, present, &[]);
                self.snapshot.control.check()?;
                self.control.check()?;
                if !more {
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn dense_presence(&self, ids: &[DocId]) -> SQLiteResult<Option<BudgetedVec<u8>>> {
        let Some(owner) = self.owner else {
            return Ok(None);
        };
        if ids.len() < 8
            || ids.windows(2).any(|pair| pair[0] >= pair[1])
            || ids[ids.len() - 1] > i64::MAX as u64
            || ids[ids.len() - 1] - ids[0] > ids.len() as u64 * 2
        {
            return Ok(None);
        }
        let mut encoded = BudgetedVec::new(self.control.memory());
        encoded.reserve(ids.len())?;
        for id in ids {
            encoded.push(*id as i64)?;
        }
        self.snapshot
            .dense_identity_presence(Family::Documents, owner, &[], &encoded, self.control)
    }
}
