//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign definitions preserve original native field and independent ACL order.

use super::super::{
    table_input::{copy, decode_acl},
    Catalog, Family, NativeRecordOwner,
};
use crate::{
    catalog::role_security::input::{relation, take},
    mvcc::native::{decode_record, NativeRecordIdentity},
};
use uqa_core::memory::Budgeted;
use uqa_storage::{
    catalog::{
        foreign_load::{CatalogForeignTableLoad, CatalogForeignTableOrder},
        input::CatalogInputLimits,
        relation_acl::METADATA_PREFIX,
        table_input::CatalogTableInput,
    },
    key_value::KeyValueRead,
    mvcc::VersionError,
    read_control::StorageReadControl,
    ForeignTableRow, RelationIdentity, StorageBackendError, StorageBackendResult,
};

impl Catalog {
    pub(in crate::catalog) fn native_foreign_table_load(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogForeignTableLoad> {
        control.check()?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other(
                "retained foreign table input requires native records".into(),
            )
        })?;
        let owner = NativeRecordOwner::Database(snapshot.database);
        let definitions = NativeRecordIdentity::new(Family::ForeignTables, owner)
            .and_then(|identity| identity.encode_prefix(&[], control))
            .map_err(VersionError::into_storage_error)?;
        let acls = NativeRecordIdentity::new(Family::Metadata, owner)
            .and_then(|identity| identity.encode_text_prefix(METADATA_PREFIX.as_bytes(), control))
            .map_err(VersionError::into_storage_error)?;
        let read = snapshot.record_read().retain(&[&definitions, &acls])?;
        let source = CatalogTableInput::new(
            read,
            (&definitions, decode),
            (&acls, decode_acl),
            limits,
            control,
        )?;
        CatalogForeignTableLoad::new(source, CatalogForeignTableOrder::Physical, control)
    }
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<ForeignTableRow>>> {
    let (_, row) = decode_record(key, value, control).map_err(VersionError::into_storage_error)?;
    let schema = copy(row[0], control)?;
    let name = copy(row[1], control)?;
    let server_name = copy(row[3], control)?;
    let columns_json = copy(row[4], control)?;
    let options_json = copy(row[5], control)?;
    let security = relation(row[6], row[7], row[8], control)?;
    let mut memory = control.memory().empty_reservation();
    let table = ForeignTableRow {
        relation: RelationIdentity {
            schema: take(schema, &mut memory),
            name: take(name, &mut memory),
        },
        server_name: take(server_name, &mut memory),
        columns_json: take(columns_json, &mut memory),
        options_json: take(options_json, &mut memory),
        security: take(security, &mut memory),
    };
    control.check()?;
    Ok(Some(Budgeted::new(table, memory)))
}
