//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Interpret retained SQL index definitions for Storage's physical incarnation binding.

use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

/// Shares the existing SQL definition decoder and catalog identity validator with `DiskANN` publication.
pub struct DiskANNIndexIdentityResolver;

impl uqa_storage::diskann_index::catalog::DiskANNIndexResolver for DiskANNIndexIdentityResolver {
    fn resolve(
        &self,
        definition: &str,
        table: [u8; 16],
        control: &StorageReadControl,
    ) -> StorageBackendResult<[u8; 16]> {
        control.check()?;
        // Charge decoded SQL nodes and collections before invoking their existing owner.
        let width = std::mem::size_of::<uqa_sql::ast::Expr>()
            .max(std::mem::size_of::<super::IndexDefinition>());
        let bytes = definition
            .len()
            .checked_mul(width)
            .and_then(|bytes| bytes.checked_mul(2))
            .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?;
        let _memory = control.memory().reserve(bytes)?;
        let definition = uqa_sql::catalog::index::stored::index_definition(Some(definition))?;
        let identity = definition.catalog.as_ref().ok_or_else(|| {
            StorageBackendError::Other("DiskANN index has no durable catalog identity".into())
        })?;
        identity
            .validate(table)
            .map_err(|error| StorageBackendError::Other(error.to_string()))?;
        control.check()?;
        Ok(identity.identity.object_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_storage::diskann_index::catalog::DiskANNIndexResolver;

    #[test]
    fn diskann_resolver_uses_stored_catalog_identity_and_existing_validation() {
        let mut definition = super::super::IndexDefinition {
            catalog: Some(uqa_sql::catalog::index::IndexCatalogIdentity {
                identity: uqa_core::catalog_identity::CatalogObjectIdentity {
                    object_id: [5; 16],
                    oid: 17001,
                },
                table_object_id: [4; 16],
                physical_key: "uqa:index:fixture".into(),
            }),
            ..Default::default()
        };
        let control = StorageReadControl::with_limit(1 << 20);
        let encoded = serde_json::to_string(&definition).unwrap();
        let resolver = DiskANNIndexIdentityResolver;
        assert_eq!(
            resolver.resolve(&encoded, [4; 16], &control).unwrap(),
            [5; 16]
        );
        assert!(resolver.resolve(&encoded, [6; 16], &control).is_err());
        assert!(resolver.resolve("{}", [4; 16], &control).is_err());
        assert!(resolver
            .resolve(&encoded, [4; 16], &StorageReadControl::with_limit(1))
            .is_err());
        for fault in 0..3 {
            let identity = definition.catalog.as_mut().unwrap();
            match fault {
                0 => identity.identity.object_id = [0; 16],
                1 => {
                    identity.identity.object_id = [5; 16];
                    identity.identity.oid = 0;
                }
                _ => {
                    identity.identity.oid = 17001;
                    identity.physical_key.clear();
                }
            }
            assert!(resolver
                .resolve(
                    &serde_json::to_string(&definition).unwrap(),
                    [4; 16],
                    &control
                )
                .is_err());
        }
        assert_eq!(control.memory().used(), 0);
        control.cancellation().cancel();
        assert!(resolver.resolve(&encoded, [4; 16], &control).is_err());
    }
}
