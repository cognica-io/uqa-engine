//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained system authority preserves complete physical input, row errors and sorted relation validation.

use super::{SystemRelation, SystemRelationSecurities, METADATA_PREFIX};
use crate::catalog::{text_records::rows, type_records::marker};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{Budgeted, MemoryBudget, OwnedMap, ProductionControl},
};
use uqa_sql::{
    catalog::{
        roles::RoleDefinitions,
        security::{
            projected_input::ScalarProtocol,
            system_relations::stored::{
                insert_stored_entry, StoredSystemAcl, SystemRelationAssembly,
            },
        },
    },
    SQLError,
};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    StorageBackendError, StorageBackendResult,
};

pub struct SystemRelationCatalogInput {
    input: ImageDecoder<SystemRelationSecurities, StorageBackendError>,
}
impl SystemRelationCatalogInput {
    pub fn new(
        catalog: Arc<dyn CatalogFacade>,
        roles: RoleDefinitions,
        limits: CatalogInputLimits,
        mutations: &MemoryBudget,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        limits.validate()?;
        let retained = control.clone();
        let mutations = mutations.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                let production = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    work.cancellation,
                );
                let records = rows(
                    catalog.as_ref(),
                    METADATA_PREFIX,
                    limits,
                    &retained,
                    &mut work,
                )
                .await?;
                let mut grouped = OwnedMap::new();
                for row in &*records {
                    work.checkpoint().await?;
                    production.check()?;
                    let (name, column) = row
                        .name
                        .strip_prefix(METADATA_PREFIX)
                        .and_then(|suffix| suffix.split_once(':'))
                        .ok_or_else(|| {
                            StorageBackendError::Other("invalid system ACL metadata key".into())
                        })?;
                    let relation = SystemRelation::from_qualified_name(name).ok_or_else(|| {
                        StorageBackendError::Other("unknown system ACL relation".into())
                    })?;
                    let stored = decode(&row.value, &production, &mut work).await?;
                    let key = (relation.namespace(), relation.name());
                    if !grouped.contains_key(&key) {
                        grouped.insert_budgeted(
                            key,
                            SystemRelationAssembly::new(relation),
                            retained.memory(),
                        )?;
                    }
                    let (entry, memory) = stored.into_parts();
                    grouped
                        .get_mut(&key)
                        .expect("system relation group")
                        .insert(column, Budgeted::new(entry, memory), &production)
                        .map_err(sql_error)?;
                }
                let mut registry = SystemRelationSecurities::with_budget(retained.memory());
                for ((schema, name), group) in grouped {
                    work.checkpoint().await?;
                    let relation =
                        SystemRelation::at(schema, name).expect("validated system relation");
                    let value = group
                        .finish(&production, &mut work)
                        .await
                        .map_err(sql_error)?;
                    value
                        .validate_from_input(relation, &roles, &production, &mut work)
                        .await
                        .map_err(sql_error)?;
                    insert_stored_entry(&mut registry, relation, value, &production)
                        .map_err(sql_error)?;
                }
                production.check()?;
                Ok(registry.into_mutation_budget(&mutations))
            },
        )?;
        Ok(Self { input })
    }
    pub fn advance(
        &mut self,
        steps: usize,
    ) -> StorageBackendResult<Option<SystemRelationSecurities>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}
async fn decode(
    json: &str,
    production: &ProductionControl<'_>,
    work: &mut uqa_core::image_io::AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<uqa_sql::catalog::security::system_relations::SystemAcl>> {
    let marker = marker::field_from(json, "system_acl_format", production, work).await?;
    if !marker.present {
        return Err(StorageBackendError::Other(
            "system ACL requires initial catalog migration".into(),
        ));
    }
    let keys = marker::protocol::keys(production)?;
    let stored = StoredSystemAcl::from_stored_value_json(
        json,
        ScalarProtocol {
            number: keys.number,
            raw: keys.raw,
        },
        production,
    )
    .map_err(decode_error)?
    .into_budgeted()
    .unwrap_or_else(|_| unreachable!("admitted system ACL"));
    if stored.system_acl_format != 1 {
        return Err(StorageBackendError::Other(format!(
            "unsupported system ACL format {}",
            stored.system_acl_format
        )));
    }
    if stored.entry.revision == [0; 16] {
        return Err(StorageBackendError::Other(
            "invalid system ACL tuple identity".into(),
        ));
    }

    let (stored, memory) = stored.into_parts();
    Ok(Budgeted::new(stored.entry, memory))
}

fn decode_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Internal(message) => {
            StorageBackendError::Serde(<serde_json::Error as serde::de::Error>::custom(message))
        }
        error => sql_error(error),
    }
}
fn sql_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Internal(message) => StorageBackendError::Other(message),
        SQLError::Cancelled(error) => error.into(),
        error => StorageBackendError::backend("system relation ACL", error),
    }
}

#[cfg(test)]
mod tests;
