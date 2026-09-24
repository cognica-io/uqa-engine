//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native notification publication occupies one metadata row under the stable data namespace.

use super::{
    decode_record, encode_row, NativeRecordFamily, NativeRecordIdentity, NativeRecordNamespace,
    NativeRecordOwner,
};
use rusqlite::types::ValueRef;
use std::sync::Arc;
use uqa_core::memory::BudgetedVec;
use uqa_storage::{
    mvcc::{
        NotificationRecordLayout, SharedRecordValue, VersionError, VersionResult,
        NOTIFICATION_PUBLICATION_KEY,
    },
    notifications::{NotificationPublication, NotificationPublicationView},
    read_control::StorageReadControl,
};

impl NotificationRecordLayout for NativeRecordNamespace {
    fn key(&self, control: &StorageReadControl) -> VersionResult<BudgetedVec<u8>> {
        NativeRecordIdentity::new(
            NativeRecordFamily::Metadata,
            NativeRecordOwner::Database(self.0),
        )?
        .encode_key(
            &[ValueRef::Text(NOTIFICATION_PUBLICATION_KEY.as_bytes())],
            control,
        )
    }

    fn encode(
        &self,
        publication: &NotificationPublication,
        control: &StorageReadControl,
    ) -> VersionResult<SharedRecordValue> {
        encode_row(
            &[
                ValueRef::Text(NOTIFICATION_PUBLICATION_KEY.as_bytes()),
                ValueRef::Text(publication.bytes()),
            ],
            control,
        )
        .map(Arc::new)
    }

    fn decode<'a>(
        &self,
        key: &[u8],
        record: &'a [u8],
        control: &StorageReadControl,
    ) -> VersionResult<NotificationPublicationView<'a>> {
        if key != self.key(control)?.as_ref() {
            return Err(VersionError::InvalidEncoding(
                "invalid native notification publication key",
            ));
        }
        let (_, values) = decode_record(key, record, control)?;
        let ValueRef::Text(bytes) = values[1] else {
            return Err(VersionError::InvalidEncoding(
                "native notification publication is not text",
            ));
        };
        NotificationPublicationView::decode(bytes, control)
    }
}
