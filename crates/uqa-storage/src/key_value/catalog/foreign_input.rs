//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign records and independent ACLs retain one original Key/Value view.

mod row;

use super::{KeyValueCatalog, TAG_FOREIGN_TABLE, TAG_METADATA};
use crate::{
    catalog::{
        foreign_load::{CatalogForeignTableLoad, CatalogForeignTableOrder},
        input::CatalogInputLimits,
        table_input::CatalogTableInput,
    },
    read_control::StorageReadControl,
    ForeignTableRow, StorageBackendError, StorageBackendResult,
};
use uqa_core::memory::Budgeted;

impl KeyValueCatalog {
    pub(super) fn foreign_table_load(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogForeignTableLoad> {
        control.check()?;
        if !self.store.transaction_model().is_versioned() {
            return Err(StorageBackendError::Other(
                "retained catalog input requires a versioned read owner".into(),
            ));
        }
        let read = crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            read.retain(&[&[TAG_FOREIGN_TABLE], &[TAG_METADATA]])
        })?;
        let source = CatalogTableInput::new_filtered(
            read,
            (&[TAG_FOREIGN_TABLE], decode),
            (&[TAG_METADATA], super::table_input::acl),
            super::table_input::select_acl,
            limits,
            control,
        )?;
        CatalogForeignTableLoad::new(source, CatalogForeignTableOrder::Relation, control)
    }
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<ForeignTableRow>>> {
    let relation = super::keys::decode_relation_key_with_control(key, control)?;
    row::row(relation, value, control).map(Some)
}
