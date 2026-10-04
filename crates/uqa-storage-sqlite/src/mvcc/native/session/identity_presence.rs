//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compact ordered identity sets can share one source metadata cursor.

use super::{
    Family, NativeRecordIdentity, NativeRecordOwner, NativeSnapshot, Result, StorageReadControl,
    ValueRef, VersionError,
};
use uqa_core::memory::BudgetedVec;

impl NativeSnapshot {
    /// Return source presence on the captured view for a bounded dense page. The family key must end in one integer after `components`; sparse or unordered inputs keep point reads.
    pub(crate) fn dense_identity_presence(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        components: &[ValueRef<'_>],
        ids: &[i64],
        control: &StorageReadControl,
    ) -> Result<Option<BudgetedVec<u8>>> {
        self.control.check()?;
        control.check()?;
        if !(8..=256).contains(&ids.len())
            || ids.windows(2).any(|pair| pair[0] >= pair[1])
            || ids[ids.len() - 1]
                .checked_sub(ids[0])
                .and_then(|span| usize::try_from(span).ok())
                .is_none_or(|span| span > ids.len() * 2)
        {
            return Ok(None);
        }
        let identity = NativeRecordIdentity::new(family, owner)?;
        let prefix = identity.encode_prefix(components, control)?;
        let mut lower = BudgetedVec::new(control.memory());
        lower.extend_from_slice(components)?;
        lower.push(ValueRef::Integer(ids[0].saturating_sub(1)))?;
        let after = (ids[0] != i64::MIN)
            .then(|| identity.encode_key(&lower, control))
            .transpose()?;
        let mut flags = BudgetedVec::new(control.memory());
        flags.reserve(ids.len())?;
        for _ in ids {
            flags.push(0)?;
        }
        let last = ids[ids.len() - 1];
        let limit = ids.len() * 2 + 2;
        let mut visited = 0;
        let mut surpassed = false;
        self.view.visit_keys(
            &prefix,
            after.as_deref(),
            limit,
            control,
            &mut |key, record| {
                self.control.check()?;
                visited += 1;
                let mut id = None;
                NativeRecordIdentity::visit_key_components(key, control, |column, value| {
                    if column == components.len() {
                        id = Some(value.as_i64().map_err(|_| {
                            VersionError::InvalidEncoding("source identity is not an integer")
                        })?);
                    }
                    Ok(())
                })?;
                let id = id.ok_or(VersionError::InvalidEncoding(
                    "source key is missing its identity",
                ))?;
                if id > last {
                    surpassed = true;
                    return Ok(false);
                }
                if record.live {
                    if let Ok(position) = ids.binary_search(&id) {
                        flags[position] = 1;
                    }
                }
                Ok(true)
            },
        )?;
        control.check()?;
        self.control.check()?;
        Ok((surpassed || visited < limit).then_some(flags))
    }
}
