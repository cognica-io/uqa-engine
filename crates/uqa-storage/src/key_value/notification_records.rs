//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The bounded notification slot uses ordinary versioned metadata addressing.

use crate::{
    mvcc::{
        NotificationRecordLayout, SharedRecordValue, VersionError, VersionResult,
        NOTIFICATION_PUBLICATION_KEY,
    },
    notifications::{NotificationPublication, NotificationPublicationView},
    read_control::StorageReadControl,
};
use uqa_core::memory::BudgetedVec;

pub struct KeyValueNotificationRecords;

impl NotificationRecordLayout for KeyValueNotificationRecords {
    fn key(&self, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>> {
        control.check()?;
        let mut key = BudgetedVec::new(control.memory());
        key.push(super::TAG_METADATA)?;
        key.extend_from_slice(
            &u32::try_from(NOTIFICATION_PUBLICATION_KEY.len())
                .map_err(|_| VersionError::InvalidEncoding("notification key is too long"))?
                .to_be_bytes(),
        )?;
        key.extend_from_slice(NOTIFICATION_PUBLICATION_KEY.as_bytes())?;
        Ok(key)
    }

    fn encode(
        &self,
        publication: &NotificationPublication,
        control: &StorageReadControl,
    ) -> VersionResult<SharedRecordValue> {
        control.check()?;
        Ok(publication.shared_bytes())
    }

    fn decode<'a>(
        &self,
        key: &[u8],
        record: &'a [u8],
        control: &StorageReadControl,
    ) -> VersionResult<NotificationPublicationView<'a>> {
        if key != self.key(control)?.as_ref() {
            return Err(VersionError::InvalidEncoding(
                "invalid notification metadata key",
            ));
        }
        NotificationPublicationView::decode(record, control)
    }
}
