//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{DefaultAclKey, DefaultAclRow, DefaultAcls};
use std::sync::Arc;
use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{BudgetedMap, MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{catalog::roles::RoleDefinitions, SQLError};
use uqa_storage::{
    catalog::input::CatalogInputLimits, read_control::StorageReadControl, CatalogFacade,
    StorageBackendError, StorageBackendResult,
};

const PREFIX: &str = "uqa.default_acl.v1:";

fn metadata_key(key: DefaultAclKey) -> String {
    format!(
        "{PREFIX}{}:{}:{}",
        key.owner.oid,
        key.namespace,
        key.object.code()
    )
}

#[derive(serde::Serialize)]
struct Stored<'a> {
    owner: uqa_core::catalog_role::RoleIdentity,
    namespace: u32,
    object: u8,
    oid: u32,
    acl: &'a [uqa_core::catalog_role::BoundAclEntry<
        uqa_sql::catalog::security::default_acl::DefaultRights,
    >],
}

pub(super) fn write(
    catalog: &dyn CatalogFacade,
    key: DefaultAclKey,
    value: Option<&DefaultAclRow>,
) -> Result<(), SQLError> {
    let key_text = metadata_key(key);
    let change = if let Some(value) = value {
        if value.temporary {
            return Ok(());
        }
        let record = Stored {
            owner: key.owner,
            namespace: key.namespace,
            object: u8::try_from(key.object.code()).expect("ASCII catalog class"),
            oid: value.oid,
            acl: &value.acl,
        };
        let json = serde_json::to_string(&record)
            .map_err(|error| SQLError::Internal(format!("serialize default ACL: {error}")))?;
        catalog.set_metadata(&key_text, &json)
    } else {
        catalog.delete_metadata(&key_text)
    };
    change.map_err(|error| uqa_sql::catalog::errors::storage_error("persist default ACL", &error))
}

/// A fixed transaction refresh sees committed changes to other templates and retains its own inserts, updates, deletions and temporary namespace.
pub fn merge_private(
    catalog: Option<&dyn CatalogFacade>,
    current: &DefaultAcls,
    committed: &DefaultAcls,
) -> StorageBackendResult<DefaultAcls> {
    let mut merged = current.temporary_only();
    merged.replace_durable(committed.clone());
    if let Some(catalog) = catalog {
        for (&key, row) in current.iter() {
            if !row.temporary
                && !current.shares_definition(&key, committed)
                && catalog.metadata_has_private_changes(&metadata_key(key))?
            {
                merged
                    .insert(key, row.clone())
                    .map_err(|error| StorageBackendError::backend("default ACL", error))?;
            }
        }
        for key in committed.keys() {
            if !current.contains_key(key)
                && catalog.metadata_has_private_changes(&metadata_key(*key))?
            {
                merged
                    .remove(key)
                    .map_err(|error| StorageBackendError::backend("default ACL", error))?;
            }
        }
    }
    Ok(merged)
}

struct Decoder<'a> {
    registry: DefaultAcls,
    oids: BudgetedMap<u32, ()>,
    roles: &'a RoleDefinitions,
}

impl Decoder<'_> {
    fn entry(
        &mut self,
        name: &str,
        text: &str,
        control: &ProductionControl<'_>,
    ) -> StorageBackendResult<()> {
        let key = self
            .registry
            .read_stored(text, self.roles, control)
            .map_err(|error| StorageBackendError::backend("default ACL", error))?;
        if name != metadata_key(key)
            || self
                .oids
                .insert(self.registry.get(&key).expect("published tuple").oid, ())?
                .is_some()
        {
            return Err(StorageBackendError::Other(
                "invalid default ACL metadata key or duplicate OID".into(),
            ));
        }
        Ok(())
    }
}

pub fn restore(
    catalog: &dyn CatalogFacade,
    roles: &RoleDefinitions,
    memory: &MemoryBudget,
) -> StorageBackendResult<DefaultAcls> {
    let token = CancellationToken::new();
    let control = ProductionControl::new(memory, &token, &token);
    let mut decoder = Decoder {
        registry: DefaultAcls::with_budget(memory),
        oids: BudgetedMap::new(memory),
        roles,
    };
    for (name, text) in catalog.metadata_with_prefix(PREFIX)? {
        decoder.entry(&name, &text, &control)?;
    }
    Ok(decoder.registry)
}

pub struct DefaultAclInput(ImageDecoder<DefaultAcls, StorageBackendError>);
impl DefaultAclInput {
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
        Ok(Self(ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut reader| async move {
                let mut work = reader.input();
                let production = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    work.cancellation,
                );
                let records = crate::catalog::text_records::rows(
                    catalog.as_ref(),
                    PREFIX,
                    limits,
                    &retained,
                    &mut work,
                )
                .await?;
                let (rows, _memory) = records.into_parts();
                let mut decoder = Decoder {
                    registry: DefaultAcls::with_budget(retained.memory()),
                    oids: BudgetedMap::new(retained.memory()),
                    roles: &roles,
                };
                for row in rows {
                    work.checkpoint().await?;
                    decoder.entry(&row.name, &row.value, &production)?;
                }
                Ok(decoder.registry.into_mutation_budget(&mutations))
            },
        )?))
    }

    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<DefaultAcls>> {
        self.0
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}
