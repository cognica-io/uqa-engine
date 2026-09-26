//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native field-guard identities adapt common empty-field maintenance without bypassing row projection.

use rusqlite::types::ValueRef;
use uqa_core::memory::BudgetedVec;
use uqa_storage::mvcc::{VectorFieldGuard, VectorFieldGuardLayout, VersionResult};
use uqa_storage::read_control::StorageReadControl;

use super::{
    invalid, NativeRecord, NativeRecordFamily as Family, NativeRecordIdentity, NativeRecordOwner,
};
use crate::mvcc::SQLiteRecordStore;

pub(crate) const VECTOR_FIELD_GUARD_PREFIX: &str = "vector_field_guard::";

impl VectorFieldGuardLayout for SQLiteRecordStore {
    fn prefix(&self, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>> {
        let Some(namespace) = self.native else {
            return uqa_storage::key_value::KeyValueVectorFieldGuards.prefix(control);
        };
        NativeRecordIdentity::new(Family::Metadata, NativeRecordOwner::Database(namespace.0))?
            .encode_text_prefix(VECTOR_FIELD_GUARD_PREFIX.as_bytes(), control)
    }

    fn reference(
        &self,
        key: &[u8],
        control: &StorageReadControl,
    ) -> VersionResult<Option<VectorFieldGuard>> {
        let Some(namespace) = self.native else {
            return uqa_storage::key_value::KeyValueVectorFieldGuards.reference(key, control);
        };
        let mut result = None;
        let identity = NativeRecordIdentity::visit_key_components(key, control, |_, component| {
            let name = component
                .as_str()
                .map_err(|_| invalid("vector field guard is not TEXT"))?;
            let suffix = name
                .strip_prefix(VECTOR_FIELD_GUARD_PREFIX)
                .ok_or_else(|| invalid("foreign vector field guard"))?;
            let (hex, reference) = if let Some(hex) = suffix.strip_suffix("::references") {
                (hex, true)
            } else if let Some(hex) = suffix.strip_suffix("::lifetime") {
                (hex, false)
            } else {
                return Err(invalid("invalid vector field guard suffix"));
            };
            if hex.len() % 2 != 0 {
                return Err(invalid("invalid vector field guard encoding"));
            }
            let mut vectors = BudgetedVec::new(control.memory());
            for pair in hex.as_bytes().chunks_exact(2) {
                control.check()?;
                let digit = |byte| match byte {
                    b'0'..=b'9' => Ok(byte - b'0'),
                    b'a'..=b'f' => Ok(byte - b'a' + 10),
                    _ => Err(invalid("invalid vector field guard hexadecimal digit")),
                };
                vectors.push((digit(pair[0])? << 4) | digit(pair[1])?)?;
            }
            let field =
                NativeRecordIdentity::visit_prefix_components(&vectors, 1, control, |_, _| Ok(()))?;
            if field.family() != Family::Vectors
                || !matches!(field.owner(), NativeRecordOwner::Object { .. })
            {
                return Err(invalid("vector field guard does not name a vector field"));
            }
            if !reference {
                return Ok(());
            }
            let owner = NativeRecordOwner::Database(namespace.0);
            let references = NativeRecord::encode(
                Family::Metadata,
                owner,
                &[ValueRef::Text(name.as_bytes()), ValueRef::Text(b"1")],
                control,
            )?;
            let mut lifetime = BudgetedVec::new(control.memory());
            lifetime.extend_from_slice(&name.as_bytes()[..name.len() - "::references".len()])?;
            lifetime.extend_from_slice(b"::lifetime")?;
            let lifetime = NativeRecord::encode(
                Family::Metadata,
                owner,
                &[ValueRef::Text(&lifetime), ValueRef::Text(b"1")],
                control,
            )?;
            result = Some(VectorFieldGuard {
                lifetime: lifetime.key,
                references: references.key,
                reference_value: references.row,
                vectors,
            });
            Ok(())
        })?;
        if identity.family() != Family::Metadata
            || identity.owner() != NativeRecordOwner::Database(namespace.0)
        {
            return Err(invalid("foreign vector field guard owner"));
        }
        Ok(result)
    }
}
