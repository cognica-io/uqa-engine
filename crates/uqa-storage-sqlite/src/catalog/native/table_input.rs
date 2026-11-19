//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Table definitions and ACL metadata share the original native snapshot and record decoders.

use super::{string_ref, Catalog, Family, NativeRecordOwner};
use crate::catalog::role_security::input::{relation, take};
use crate::mvcc::native::{decode_record, NativeRecordIdentity};
use rusqlite::types::ValueRef;
use uqa_core::memory::{Budgeted, ProductionControl};
use uqa_storage::{
    catalog::{
        input::{table, CatalogInputLimits},
        relation_acl::METADATA_PREFIX,
        table_input::{CatalogTableAcl, CatalogTableInput},
    },
    key_value::KeyValueRead,
    mvcc::VersionError,
    read_control::StorageReadControl,
    RelationIdentity, StorageBackendError, StorageBackendResult, TableSchema,
};

impl Catalog {
    pub(in crate::catalog) fn native_table_input(
        &self,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogTableInput> {
        control.check()?;
        let snapshot = self.conn.native_snapshot()?.ok_or_else(|| {
            StorageBackendError::Other("retained table input requires native records".into())
        })?;
        let definitions = NativeRecordIdentity::family_prefix(Family::Tables, control)
            .map_err(VersionError::into_storage_error)?;
        let acls = NativeRecordIdentity::new(
            Family::Metadata,
            NativeRecordOwner::Database(snapshot.database),
        )
        .and_then(|identity| identity.encode_text_prefix(METADATA_PREFIX.as_bytes(), control))
        .map_err(VersionError::into_storage_error)?;
        let read = snapshot.record_read().retain(&[&definitions, &acls])?;
        CatalogTableInput::new(
            read,
            (&definitions, decode_definition),
            (&acls, decode_acl),
            limits,
            control,
        )
    }
}

fn decode_definition(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<TableSchema>>> {
    let (_, row) = decode_record(key, value, control).map_err(VersionError::into_storage_error)?;
    let mut memory = control.memory().empty_reservation();
    let schema = copy(row[0], control)?;
    let name = copy(row[1], control)?;
    let analyzer_json = copy(row[3], control)?;
    let fts_fields = table::field_names(string_ref(row[4])?.as_bytes(), control)?;
    let vector_fields = table::vector_fields(string_ref(row[5])?.as_bytes(), control)?;
    let columns_json = if row[6] == ValueRef::Null {
        Budgeted::new(String::new(), control.memory().empty_reservation())
    } else {
        copy(row[6], control)?
    };
    let constraints_json = copy(row[7], control)?;
    let storage_generation = super::tables::identity(row[8])?;
    let object_id = super::tables::identity(row[9])?;
    let security = relation(row[10], row[11], row[12], control)?;
    control.check()?;
    Ok(Some(Budgeted::new(
        TableSchema {
            relation: RelationIdentity {
                schema: take(schema, &mut memory),
                name: take(name, &mut memory),
            },
            analyzer_json: take(analyzer_json, &mut memory),
            fts_fields: take(fts_fields, &mut memory),
            vector_fields: take(vector_fields, &mut memory),
            columns_json: take(columns_json, &mut memory),
            constraints_json: take(constraints_json, &mut memory),
            storage_generation,
            object_id,
            security: take(security, &mut memory),
        },
        memory,
    )))
}

pub(super) fn copy(
    value: ValueRef<'_>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<String>> {
    Ok(ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(string_ref(value)?)?
    .into_budgeted()
    .expect("controlled table text"))
}

pub(super) fn decode_acl(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<CatalogTableAcl>>> {
    let (_, row) = decode_record(key, value, control).map_err(VersionError::into_storage_error)?;
    CatalogTableAcl::decode(string_ref(row[0])?, string_ref(row[1])?.as_bytes(), control).map(Some)
}
