//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize durable implicit sequences and their stable foreign-column owners.
use uqa_core::RelationIdentity;
use uqa_sql::schema::sequences::implicit::{
    apply_implicit_sequence_metadata, choose_implicit_sequence_name, implicit_sequence_data_type,
};
use uqa_storage::{
    CatalogFacade, SequenceOwner, SequenceOwnerDependency, StorageBackendError,
    StorageBackendResult,
};

fn persisted_relation_names(
    catalog: &dyn CatalogFacade,
) -> StorageBackendResult<std::collections::BTreeSet<RelationIdentity>> {
    let mut relations = std::collections::BTreeSet::new();
    relations.extend(catalog.load_tables()?.into_iter().map(|row| row.relation));
    relations.extend(
        catalog
            .load_sequence_rows()?
            .into_iter()
            .map(|row| row.relation),
    );
    relations.extend(catalog.load_views()?.into_iter().map(|row| row.relation));
    relations.extend(
        catalog
            .load_foreign_tables()?
            .into_iter()
            .map(|row| row.relation),
    );
    relations.extend(
        catalog
            .load_catalog_indexes()?
            .into_iter()
            .map(|row| row.relation),
    );
    Ok(relations)
}

pub fn materialize_persisted_foreign_implicit_sequences(
    catalog: &dyn CatalogFacade,
    relation: &RelationIdentity,
    role_owner: &str,
    table_object_id: [u8; 16],
    columns: &mut [uqa_sql::ast::ColumnDef],
) -> StorageBackendResult<bool> {
    let table_name = relation.qualified_name();
    let occupied_relations = persisted_relation_names(catalog)?;
    let mut sequences = Vec::new();
    for (column_index, column) in columns.iter().enumerate() {
        let Some(auto_increment) = column.auto_increment.as_ref() else {
            continue;
        };
        if auto_increment.kind == uqa_sql::ast::AutoIncrementKind::Legacy
            || auto_increment.sequence.is_some()
        {
            continue;
        }
        let data_type = implicit_sequence_data_type(column).map_err(StorageBackendError::Other)?;
        let sequence = choose_implicit_sequence_name(
            relation,
            &column.name,
            |candidate| Ok(occupied_relations.contains(candidate)),
            StorageBackendError::Other,
        )?;
        sequences.push((column_index, data_type, sequence));
    }
    let mut changed = false;
    for (column_index, data_type, sequence) in sequences {
        let column = &mut columns[column_index];
        let auto_increment = column.auto_increment.as_ref().ok_or_else(|| {
            StorageBackendError::Other(format!(
                "implicit sequence column `{table_name}`.`{}` lost its generation metadata",
                column.name
            ))
        })?;
        let column_object_id = column.object_id.ok_or_else(|| {
            StorageBackendError::Other(format!(
                "column `{table_name}`.`{}` has no object identity during implicit sequence migration",
                column.name
            ))
        })?;
        let mut state = crate::catalog::sequence::SequenceState::initial(1, 1, data_type);
        state.owner = Some(SequenceOwner {
            table_object_id,
            column_object_id,
            dependency: if auto_increment.is_identity() {
                SequenceOwnerDependency::Internal
            } else {
                SequenceOwnerDependency::Automatic
            },
        });
        let object_id =
            crate::catalog::identity::new_nonzero_catalog_identity("sequence", "object identity")?;
        state.definition_generation = object_id;
        let security = crate::catalog::security::SequenceSecurity {
            role_owner: role_owner.to_string(),
            acl: None,
        };
        if !catalog.create_sequence_row(&crate::catalog::sequence::sequence_row(
            &sequence,
            object_id,
            state,
            uqa_sql::ast::RelationPersistence::Permanent,
            &security,
        )?)? {
            return Err(StorageBackendError::Other(format!(
                "cannot migrate implicit sequence `{sequence}` because that relation name already exists"
            )));
        }
        apply_implicit_sequence_metadata(&table_name, column, sequence)
            .map_err(StorageBackendError::Other)?;
        changed = true;
    }
    Ok(changed)
}
