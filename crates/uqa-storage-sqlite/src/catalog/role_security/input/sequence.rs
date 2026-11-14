//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence authority cells preserve native role classes and nullable SQL ACLs.

use super::{json_error, nonnull, take, Result, SQLiteError};
use rusqlite::types::ValueRef;
use uqa_core::{
    catalog_acl::input::sequence,
    catalog_sequence::{BoundSequenceSecurity, LegacySequenceSecurity},
    memory::{Budgeted, ProductionControl},
};
use uqa_storage::{read_control::StorageReadControl, SequenceSecurityRow};

pub(in crate::catalog) fn read(
    owner: ValueRef<'_>,
    acl: Option<&str>,
    control: &StorageReadControl,
) -> Result<Budgeted<SequenceSecurityRow>> {
    control.check()?;
    let mut memory = control.memory().empty_reservation();
    let row = match owner {
        ValueRef::Text(_) => {
            let owner = ProductionControl::new(
                control.memory(),
                control.cancellation(),
                control.cancellation(),
            )
            .copy_text(super::super::super::native::string_ref(owner)?)
            .map_err(uqa_storage::StorageBackendError::from)?
            .into_budgeted()
            .expect("admitted sequence owner");
            let acl = acl
                .map(|text| {
                    nonnull(
                        sequence::legacy_entries(
                            text.as_bytes(),
                            control.memory(),
                            control.cancellation(),
                        )
                        .map_err(json_error)?,
                    )
                })
                .transpose()?
                .unwrap_or_else(|| Budgeted::new(None, control.memory().empty_reservation()));
            SequenceSecurityRow::Legacy(LegacySequenceSecurity {
                role_owner: take(owner, &mut memory),
                acl: take(acl, &mut memory),
            })
        }
        ValueRef::Blob(bytes) => {
            let owner = super::super::decode_identity(bytes)?;
            let acl = acl
                .map(|text| {
                    nonnull(
                        sequence::entries(
                            text.as_bytes(),
                            control.memory(),
                            control.cancellation(),
                        )
                        .map_err(json_error)?,
                    )
                })
                .transpose()?
                .unwrap_or_else(|| Budgeted::new(None, control.memory().empty_reservation()));
            SequenceSecurityRow::Bound(BoundSequenceSecurity {
                role_owner: owner,
                acl: take(acl, &mut memory),
            })
        }
        _ => {
            return Err(SQLiteError::StorageBackend(
                "sequence owner has an invalid storage class".into(),
            ))
        }
    };
    control.check()?;
    Ok(Budgeted::new(row, memory))
}
