//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persistent row validation transfers admitted names and bound ACL buffers into their native owners.

use super::{
    copy_relation, sql_error, AsyncImageInput, Budgeted, ProductionControl, RoleDefinitions,
    SQLError, SequenceCatalog, SequenceRecord, StorageBackendError, StorageBackendResult,
};
use uqa_sql::{ast::RelationPersistence, catalog::security::BoundSequenceSecurity};
use uqa_storage::{SequenceRow, SequenceSecurityRow};

pub(super) async fn insert(
    result: &mut SequenceCatalog,
    row: Budgeted<SequenceRow>,
    roles: &RoleDefinitions,
    control: &ProductionControl<'_>,
    input: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    control.check()?;
    if row.object_id == [0; 16] {
        return Err(StorageBackendError::Other(format!(
            "corrupt sequence `{}` has no object identity",
            row.relation.qualified_name()
        )));
    }
    if result.durable_objects.get(&row.object_id).is_some() {
        return Err(StorageBackendError::Other(format!(
            "corrupt sequence `{}` has a duplicate object identity",
            row.relation.qualified_name()
        )));
    }
    let persistence = match row.persistence.as_str() {
        "p" => RelationPersistence::Permanent,
        "u" => RelationPersistence::Unlogged,
        other => {
            return Err(StorageBackendError::Other(format!(
                "corrupt sequence `{}` persistence `{other}`",
                row.relation.qualified_name()
            )))
        }
    };
    let authority_error = |message: String| {
        StorageBackendError::Other(format!(
            "corrupt sequence `{}` has invalid security metadata: {message}",
            row.relation.qualified_name()
        ))
    };
    let SequenceSecurityRow::Bound(authority) = &row.security else {
        return Err(authority_error(
            "sequence security requires initial catalog migration".into(),
        ));
    };
    BoundSequenceSecurity::validate_stored_from_input(authority, roles, control, input)
        .await
        .map_err(|error| match error {
            SQLError::Internal(message) => authority_error(message),
            error => sql_error(error),
        })?;
    let state = crate::catalog::sequence::restoration::sequence_state(&row)?;
    let record = SequenceRecord {
        state,
        object_id: row.object_id,
        persistence,
        catalog_oid: None,
    };
    let authority_key = copy_relation(&row.relation, input).await?;
    let (row, mut memory) = row.into_parts();
    let names = memory.split(row.relation.schema.capacity() + row.relation.name.capacity());
    let SequenceSecurityRow::Bound(authority) = row.security else {
        unreachable!("validated bound sequence authority")
    };
    let authority_memory = memory.split(authority.acl.as_ref().map_or(0, |acl| {
        acl.capacity()
            * size_of::<uqa_sql::catalog::security::sequence_binding::BoundSequenceAclEntry>()
    }));
    let security = Budgeted::new(BoundSequenceSecurity::from_row(authority), authority_memory);
    let relation = Budgeted::new(row.relation, names);
    drop((row.options, row.persistence));
    drop(memory);
    // Ordinary restoration replaces a repeated relation but still rejects every repeated object identity in its complete source. Keep that seen set independent of the live name map.
    result
        .durable_objects
        .insert(record.object_id, record.state.definition_generation, 0)?;
    result
        .security
        .insert_stored_entry(authority_key, persistence, security, control)
        .map_err(sql_error)?;
    result
        .records
        .insert_image_entry(relation, record)
        .map_err(sql_error)?;
    Ok(())
}
