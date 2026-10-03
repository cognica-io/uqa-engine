//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize one auxiliary publication slot against fresh committed state without replaying evaluated user writes.

use sha2::{Digest, Sha256};
use std::sync::Arc;
use uqa_core::memory::BudgetedVec;

use super::{
    key::RecordKey, CommittedRecordSnapshot, PreparedRecordCommit, PreparedRecordWrite,
    SharedRecordValue, VersionError, VersionResult,
};
use crate::{
    notifications::{NotificationPublication, NotificationPublicationView},
    read_control::StorageReadControl,
};

pub const NOTIFICATION_PUBLICATION_KEY: &str = "_uqa_notification_publication";

/// Provider-owned metadata addressing and lossless row encoding. Native adapters must use their stable data namespace, independent of restored transaction history.
pub trait NotificationRecordLayout: Send + Sync {
    fn key(&self, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>>;
    fn encode(
        &self,
        publication: &NotificationPublication,
        control: &StorageReadControl,
    ) -> VersionResult<SharedRecordValue>;
    fn decode<'a>(
        &self,
        key: &[u8],
        record: &'a [u8],
        control: &StorageReadControl,
    ) -> VersionResult<NotificationPublicationView<'a>>;
}

pub(super) struct NotificationEffect {
    key: RecordKey,
    record: Option<SharedRecordValue>,
    publication: [u8; 32],
}

impl NotificationEffect {
    pub(super) fn publish(
        publication: &NotificationPublication,
        layout: &dyn NotificationRecordLayout,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<Self>> {
        control.check()?;
        Ok(Arc::new(Self {
            key: RecordKey::new(&layout.key(control)?, control.memory())?,
            record: Some(layout.encode(publication, control)?),
            publication: publication.fingerprint(),
        }))
    }

    pub(super) fn acknowledge(
        publication: [u8; 32],
        layout: &dyn NotificationRecordLayout,
        control: &StorageReadControl,
    ) -> VersionResult<Arc<Self>> {
        control.check()?;
        Ok(Arc::new(Self {
            key: RecordKey::new(&layout.key(control)?, control.memory())?,
            record: None,
            publication,
        }))
    }

    pub(super) fn same_publication(&self, other: &Self) -> bool {
        self.key.bytes() == other.key.bytes()
            && self.record.is_some() == other.record.is_some()
            && self.publication == other.publication
    }

    pub(super) fn seal(&self, original: super::CommitFingerprint) -> super::CommitFingerprint {
        let mut digest = Sha256::new();
        digest.update(b"UQA prepared notification effect 1");
        digest.update(original);
        digest.update((self.key.bytes().len() as u64).to_be_bytes());
        digest.update(self.key.bytes());
        digest.update([u8::from(self.record.is_some())]);
        digest.update(self.publication);
        digest.finalize().into()
    }
}

pub(super) fn resolve(
    original: &PreparedRecordCommit,
    effect: &NotificationEffect,
    current: &dyn CommittedRecordSnapshot,
    layout: &dyn NotificationRecordLayout,
    control: &StorageReadControl,
) -> VersionResult<PreparedRecordCommit> {
    control.check()?;
    let key = effect.key.bytes();
    if original.contains_key(key, control)? {
        return Err(VersionError::InvalidEncoding(
            "notification publication conflicts with an ordinary record write",
        ));
    }
    let latest = current.get(key, control)?;
    let pending = latest
        .as_ref()
        .and_then(super::RecordVersion::value)
        .map(|value| layout.decode(key, value, control))
        .transpose()?;
    let replace = if effect.record.is_some() {
        if pending.is_some() {
            return Err(VersionError::InvalidEncoding(
                "committed notification publication must be acknowledged before replacement",
            ));
        }
        true
    } else {
        pending.is_some_and(|publication| publication.fingerprint() == effect.publication)
    };
    // The publication takes its place in key order, which a spilled batch keeps.
    let mut publication = replace.then(|| {
        PreparedRecordWrite::from_shared(
            effect.key.clone(),
            latest.as_ref().map(super::RecordVersion::sequence),
            effect.record.clone(),
        )
    });
    let mut writes = super::commit::PreparedWritesBuilder::like(original, control)?;
    let mut originals = original.writes();
    while let Some(write) = originals.next(control)? {
        control.check()?;
        if write.key() > key {
            if let Some(publication) = publication.take() {
                writes.push(publication, control)?;
            }
        }
        writes.push(write, control)?;
    }
    if let Some(publication) = publication {
        writes.push(publication, control)?;
    }
    Ok(writes
        .finish(control)?
        .resolved(original, current.sequence()))
}
