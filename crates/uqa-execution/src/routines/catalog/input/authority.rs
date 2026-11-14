//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete raw authority conversion precedes every typed routine declaration.

use super::{RoutineCatalogDocument, StorageBackendError, StorageBackendResult};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{BudgetedVec, MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    catalog::roles::RoleDefinitions,
    routines::security::binding::{
        bind_legacy_authority_from_input, input::read_legacy_acl_with_work, BoundRoutineAuthority,
    },
    SQLError,
};

/// One complete generic catalog with all raw authority checks finished. Legacy overrides retain native identities and ACL allocations; current formats keep their original generic authority for subsequent typed construction.
pub struct RoutineCatalogAuthority {
    catalog: RoutineCatalogDocument,
    legacy: BudgetedVec<Produced<BoundRoutineAuthority>>,
    roles: RoleDefinitions,
}

impl RoutineCatalogAuthority {
    pub fn catalog(&self) -> &RoutineCatalogDocument {
        &self.catalog
    }

    /// Legacy overrides follow the original ordered group/overload traversal. Current formats have no override; empty groups remain in the original catalog for later name/schema validation.
    pub fn legacy_authorities(&self) -> impl ExactSizeIterator<Item = &BoundRoutineAuthority> {
        self.legacy.iter().map(|value| &**value)
    }

    pub(super) fn into_parts(
        self,
    ) -> (
        RoutineCatalogDocument,
        BudgetedVec<Produced<BoundRoutineAuthority>>,
        RoleDefinitions,
    ) {
        (self.catalog, self.legacy, self.roles)
    }
}

impl RoutineCatalogDocument {
    /// Finish every current-field presence check or legacy authority conversion before exposing the prepared catalog to typed routine construction.
    pub async fn bind_authority_from(
        self,
        roles: &RoleDefinitions,
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<RoutineCatalogAuthority> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        let mut legacy = BudgetedVec::new(memory);
        for (_, definitions) in self.definitions() {
            for definition in definitions {
                control.check()?;
                input.checkpoint().await?;
                let root = definition.root();
                if self.format() >= 2 {
                    if root.get("owner").is_none() || root.get("execute_acl").is_none() {
                        return Err(StorageBackendError::Other(
                            "current routine authority fields are missing".into(),
                        ));
                    }
                    continue;
                }
                if root.fields().is_none() {
                    return Err(StorageBackendError::Other(
                        "legacy routine definition is not an object".into(),
                    ));
                }
                let owner = root
                    .get("owner")
                    .and_then(uqa_core::json::JsonValue::as_str)
                    .ok_or_else(|| {
                        StorageBackendError::Other("legacy routine owner is missing".into())
                    })?;
                let acl =
                    read_legacy_acl_with_work(root.get("execute_acl"), &control, input).await?;
                legacy.push(
                    bind_legacy_authority_from_input(
                        owner,
                        acl.as_deref(),
                        roles,
                        self.format() == 0,
                        &control,
                        input,
                    )
                    .await
                    .map_err(binding_error)?,
                )?;
            }
        }
        control.check()?;
        Ok(RoutineCatalogAuthority {
            catalog: self,
            legacy,
            roles: roles.clone(),
        })
    }
}

pub(super) fn binding_error(error: SQLError) -> StorageBackendError {
    match error {
        SQLError::Cancelled(error) => error.into(),
        SQLError::RetainedRoutine(source) => StorageBackendError::RetainedDiagnostic(source),
        error => StorageBackendError::Other(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
