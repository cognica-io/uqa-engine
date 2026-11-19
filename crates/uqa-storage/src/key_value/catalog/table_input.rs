//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table envelopes and ACL tuples retain one original Key/Value catalog view.

use super::{KeyValueCatalog, TAG_METADATA, TAG_TABLE};
use crate::{
    catalog::{
        input::{table, CatalogInputLimits},
        relation_acl::METADATA_PREFIX,
        table_input::{CatalogTableAcl, CatalogTableInput},
        table_load::{CatalogTableLoad, CatalogTableOrder, CatalogTableValidation},
    },
    key_value::codec::read_str_ref,
    read_control::StorageReadControl,
    StorageBackendError, StorageBackendResult, TableSchema,
};
use uqa_core::memory::{Budgeted, ProductionControl};

impl KeyValueCatalog {
    pub(super) fn table_load(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogTableLoad> {
        let read = self.table_read(control)?;
        let source = physical_input(read.clone(), limits, control)?;
        let validation = self
            .store
            .identifier_allocator()
            .map(|_| CatalogTableValidation {
                read,
                validate: super::super::table_owners::validate_table_with_control,
                limits,
            });
        CatalogTableLoad::new(
            source,
            CatalogTableOrder::AfterReplacements,
            validation,
            control,
        )
    }

    pub(super) fn table_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogTableInput> {
        physical_input(self.table_read(control)?, limits, control)
    }

    fn table_read(
        &self,
        control: &StorageReadControl,
    ) -> StorageBackendResult<std::sync::Arc<dyn crate::key_value::KeyValueRead + Send + Sync>>
    {
        control.check()?;
        if !self.store.transaction_model().is_versioned() {
            return Err(StorageBackendError::Other(
                "retained catalog input requires a versioned read owner".into(),
            ));
        }
        crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            read.retain(&[
                &[TAG_TABLE],
                &[TAG_METADATA],
                &[super::super::table_owners::TAG],
            ])
        })
    }
}

fn physical_input(
    read: std::sync::Arc<dyn crate::key_value::KeyValueRead + Send + Sync>,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
) -> StorageBackendResult<CatalogTableInput> {
    CatalogTableInput::new_filtered(
        read,
        (&[TAG_TABLE], definition),
        (&[TAG_METADATA], acl),
        select_acl,
        limits,
        control,
    )
}

fn definition(
    key: &[u8],
    bytes: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<TableSchema>>> {
    let relation = super::keys::decode_relation_key_with_control(key, control)?;
    let schema = table::row(bytes, control)?;
    if *relation != schema.relation {
        let production = ProductionControl::new(
            control.memory(),
            control.cancellation(),
            control.cancellation(),
        );
        let key = relation.qualified_name_with_control(&production)?;
        let stored = schema.relation.qualified_name_with_control(&production)?;
        return Err(StorageBackendError::Other(format!(
            "table catalog key `{}` disagrees with stored relation `{}`",
            key.as_str(),
            stored.as_str()
        )));
    }
    Ok(Some(schema))
}

pub(super) fn select_acl(key: &[u8], control: &StorageReadControl) -> StorageBackendResult<bool> {
    control.check()?;
    // The original ACL scan reads the first encoded string and accepts its existing suffix representation.
    Ok(read_str_ref(key, &mut 1)?.starts_with(METADATA_PREFIX))
}

pub(super) fn acl(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTableAcl>>> {
    CatalogTableAcl::decode(read_str_ref(key, &mut 1)?, value, control).map(Some)
}

#[cfg(test)]
mod tests;
