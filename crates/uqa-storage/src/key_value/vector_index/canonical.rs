//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Raw vector handles retain ordered provider visibility independently of mutation origins.

use super::{read_view, vector_field_prefix, KeyValueVectorIndex};
use crate::{
    key_value::KeyValueRead,
    mvcc::VersionError,
    read_control::StorageReadControl,
    vector_index::{decode_vector_bytes, VectorRead, VectorReadSnapshot},
    StorageBackendResult,
};
use std::sync::Arc;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryError},
    DocId,
};

struct Canonical {
    read: Arc<dyn KeyValueRead + Send + Sync>,
    prefix: BudgetedVec<u8>,
    dimensions: u32,
    control: StorageReadControl,
}

#[cfg(test)]
mod tests;

pub(super) fn capture(
    index: &KeyValueVectorIndex,
    control: &StorageReadControl,
) -> StorageBackendResult<VectorReadSnapshot> {
    control.check()?;
    let workspace = index
        .table
        .len()
        .checked_add(index.field.len())
        .and_then(|length| length.checked_add(9))
        .and_then(|length| length.checked_mul(3))
        .ok_or(MemoryError::SizeOverflow)?;
    let _memory = control.memory().reserve(workspace)?;
    let prefix = vector_field_prefix(&index.table, &index.field)?;
    let marker = super::guards::reference_key(&prefix, control)?;
    read_view(index.store.as_ref(), |read| {
        let source = Canonical {
            read: read.retain(&[&prefix, &marker])?,
            prefix: append(&prefix, &[], control)?,
            dimensions: index.dimensions,
            control: control.clone(),
        };
        Ok(
            Budgeted::new(source, control.memory().empty_reservation()).into_shared()?
                as VectorReadSnapshot,
        )
    })
}

fn invalid(message: &'static str) -> crate::StorageBackendError {
    VersionError::InvalidEncoding(message).into_storage_error()
}

fn append(
    prefix: &[u8],
    suffix: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<BudgetedVec<u8>> {
    let mut key = BudgetedVec::new(control.memory());
    key.reserve(
        prefix
            .len()
            .checked_add(suffix.len())
            .ok_or(MemoryError::SizeOverflow)?,
    )?;
    key.extend_from_slice(prefix)?;
    key.extend_from_slice(suffix)?;
    Ok(key)
}

fn identity(prefix: &[u8], key: &[u8]) -> StorageBackendResult<(DocId, u32)> {
    let suffix = key
        .strip_prefix(prefix)
        .filter(|suffix| suffix.len() == 16)
        .ok_or_else(|| invalid("invalid canonical vector key"))?;
    let document = u64::from_be_bytes(suffix[..8].try_into().expect("eight-byte identity"));
    let ordinal = u64::from_be_bytes(suffix[8..].try_into().expect("eight-byte identity"));
    Ok((
        document,
        u32::try_from(ordinal)
            .map_err(|_| invalid("canonical vector ordinal exceeds its identity range"))?,
    ))
}

pub(super) fn read_vector(
    read: &dyn KeyValueRead,
    prefix: &[u8],
    document: DocId,
    ordinal: u32,
    dimensions: u32,
    original: &StorageReadControl,
    control: &StorageReadControl,
) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
    original.check()?;
    control.check()?;
    read.control().check()?;
    let mut suffix = [0; 16];
    suffix[..8].copy_from_slice(&document.to_be_bytes());
    suffix[8..].copy_from_slice(&u64::from(ordinal).to_be_bytes());
    let key = append(prefix, &suffix, control)?;
    let bytes = usize::try_from(dimensions)
        .map_err(|_| MemoryError::SizeOverflow)?
        .checked_mul(4)
        .ok_or(MemoryError::SizeOverflow)?;
    let mut result = None;
    let mut failure = None;
    let mut seen = false;
    let read_result = read.visit_value_bounded(&key, bytes, control, &mut |value| {
        if failure.is_none() {
            let current = (|| {
                original.check()?;
                control.check()?;
                if seen {
                    return Err(invalid("canonical point read returned multiple values"));
                }
                seen = true;
                result = value
                    .map(|value| decode_vector_bytes(value, dimensions, control))
                    .transpose()?;
                Ok(())
            })();
            if let Err(error) = current {
                failure = Some(error);
            }
        }
        if failure.is_some() {
            Err(invalid("canonical point consumer rejected its value"))
        } else {
            Ok(())
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    read_result?;
    if !seen {
        return Err(invalid("canonical point read omitted its value"));
    }
    original.check()?;
    control.check()?;
    read.control().check()?;
    Ok(result)
}

impl VectorRead for Canonical {
    fn corpus_fingerprint(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<[u8; 32]>> {
        self.check_control(control)?;
        let key = super::guards::reference_key(&self.prefix, control)?;
        let fingerprint = self.read.marker_fingerprint(&key)?;
        self.check_control(control)?;
        Ok(fingerprint)
    }
    fn check_control(&self, control: &StorageReadControl) -> StorageBackendResult<()> {
        self.read.control().check()?;
        self.control.check()?;
        control.check()
    }
    fn dimensions(&self) -> u32 {
        self.dimensions
    }
    fn next_document_after(
        &self,
        after: Option<DocId>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<DocId>> {
        self.check_control(control)?;
        let after_key = after
            .map(|document| {
                let mut suffix = [u8::MAX; 16];
                suffix[..8].copy_from_slice(&document.to_be_bytes());
                append(&self.prefix, &suffix, control)
            })
            .transpose()?;
        let mut found = None;
        let mut failure = None;
        let outcome = self.read.visit_keys_after(
            &self.prefix,
            after_key.as_deref(),
            1,
            control,
            &mut |key| {
                let result = (|| {
                    self.check_control(control)?;
                    let (document, _) = identity(&self.prefix, key)?;
                    if found.is_some() || after.is_some_and(|after| document <= after) {
                        return Err(invalid("canonical vector cursor did not advance"));
                    }
                    found = Some(document);
                    Ok(())
                })();
                if let Err(error) = result {
                    failure.get_or_insert(error);
                }
                if failure.is_some() {
                    Err(invalid("canonical key consumer rejected its identity"))
                } else {
                    Ok(())
                }
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
        outcome?;
        self.check_control(control)?;
        Ok(found)
    }
    fn document_vector_count(
        &self,
        document: DocId,
        control: &StorageReadControl,
    ) -> StorageBackendResult<u64> {
        self.check_control(control)?;
        let prefix = append(&self.prefix, &document.to_be_bytes(), control)?;
        let mut count = 0_u64;
        let mut failure = None;
        let outcome = self
            .read
            .visit_keys_after(&prefix, None, usize::MAX, control, &mut |key| {
                let result = (|| {
                    self.check_control(control)?;
                    let (found, ordinal) = identity(&self.prefix, key)?;
                    if found != document || u64::from(ordinal) != count {
                        return Err(invalid("canonical vector ordinals are not contiguous"));
                    }
                    count += 1;
                    Ok(())
                })();
                if let Err(error) = result {
                    failure.get_or_insert(error);
                }
                if failure.is_some() {
                    Err(invalid("canonical count consumer rejected its identity"))
                } else {
                    Ok(())
                }
            });
        if let Some(error) = failure {
            return Err(error);
        }
        outcome?;
        self.check_control(control)?;
        Ok(count)
    }
    fn read_vector(
        &self,
        document: DocId,
        ordinal: u32,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Option<BudgetedVec<f32>>> {
        read_vector(
            &*self.read,
            &self.prefix,
            document,
            ordinal,
            self.dimensions,
            &self.control,
            control,
        )
    }
}
