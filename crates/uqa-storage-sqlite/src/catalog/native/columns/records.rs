//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select field identities from keys so unrelated document BLOBs and retrieval payloads are never hydrated.

use super::{text, Family, NativeRecordOwner, NativeSnapshot, Result, ValueRef, VersionError};
use crate::mvcc::native::NativeRecordIdentity;

impl NativeSnapshot {
    pub(super) fn visit_field_keys(
        &self,
        family: Family,
        owner: NativeRecordOwner,
        column: usize,
        field: &str,
        after: Option<&[u8]>,
        mut visit: impl FnMut(&[u8]) -> Result<bool>,
    ) -> Result<()> {
        let component = family
            .layout()
            .identity_columns
            .iter()
            .position(|candidate| *candidate == column)
            .expect("field belongs to native primary key");
        let field = text(field);
        let prefix_fields: &[ValueRef<'_>] = if component == 0 {
            std::slice::from_ref(&field)
        } else {
            &[]
        };
        let prefix = NativeRecordIdentity::new(family, owner)?
            .encode_prefix(prefix_fields, &self.control)?;
        self.view.visit_keys(
            &prefix,
            after,
            usize::MAX,
            &self.control,
            &mut |key, record| {
                if !record.live {
                    return Ok(true);
                }
                let mut selected = component == 0;
                if !selected {
                    NativeRecordIdentity::visit_key_components(
                        key,
                        &self.control,
                        |index, value| {
                            if index == component {
                                selected = value == field;
                            }
                            Ok(())
                        },
                    )?;
                }
                if selected {
                    visit(key).map_err(|error| VersionError::Storage(error.into()))
                } else {
                    Ok(true)
                }
            },
        )?;
        Ok(())
    }
}
