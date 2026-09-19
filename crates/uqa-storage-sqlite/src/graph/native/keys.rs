//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Graph names and memberships are decoded from bounded key pages without hydrating registry payloads.

use super::{owner, Family, NativeSnapshot, Result, ValueRef};
use crate::mvcc::native::NativeRecordIdentity;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::VersionError;

pub(super) fn strings(
    snapshot: &NativeSnapshot,
    family: Family,
    parts: &[ValueRef<'_>],
    column: usize,
) -> Result<Vec<String>> {
    let prefix = NativeRecordIdentity::new(family, owner(snapshot))?
        .encode_prefix(parts, &snapshot.control)?;
    let mut names = BudgetedVec::new(snapshot.control.memory());
    let mut payloads = snapshot.control.memory().empty_reservation();
    let mut after = BudgetedVec::new(snapshot.control.memory());
    loop {
        let mut last = BudgetedVec::new(snapshot.control.memory());
        snapshot.view.visit_keys(
            &prefix,
            (!after.is_empty()).then_some(&*after),
            256,
            &snapshot.control,
            &mut |key, record| {
                last.clear();
                last.extend_from_slice(key)?;
                if record.live {
                    NativeRecordIdentity::visit_key_components(
                        key,
                        &snapshot.control,
                        |component, value| {
                            if component == column {
                                let name = value.as_str().map_err(|_| {
                                    VersionError::InvalidEncoding("graph name key is not text")
                                })?;
                                payloads.grow(name.len())?;
                                names.push(name.to_owned())?;
                            }
                            Ok(())
                        },
                    )?;
                }
                Ok(true)
            },
        )?;
        if last.is_empty() {
            return Ok(names.into_parts().0);
        }
        after = last;
    }
}
