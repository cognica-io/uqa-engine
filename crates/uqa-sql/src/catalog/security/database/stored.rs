//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Database authority fields retain their generic value projection and native allocation owner.

use super::{BoundDatabaseSecurity, DatabaseAuthority, DatabasePrivileges};
use crate::{
    ast::stored_input::{invalid, InputError, Parts},
    catalog::security::projected_input::{Input, ScalarProtocol},
    SQLError,
};
use uqa_core::{
    memory::{Budgeted, MemoryBudget, Produced, ProductionControl},
    CancellationToken,
};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredSecurity<T> {
    pub database_security_format: u32,
    pub security: T,
}

impl StoredSecurity<BoundDatabaseSecurity> {
    /// Construct native fields after the record owner has validated the complete generic value, including overwritten and discarded values. Version and role checks remain at that owner's original boundary.
    pub fn from_stored_value_json(
        text: &str,
        protocol: ScalarProtocol,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        control.budget().ok_or_else(|| {
            SQLError::Internal("stored authority input requires admitted production".into())
        })?;
        let input = Input {
            control: *control,
            protocol,
        };
        let mut parts = Parts::new(*control);
        let (mut version, mut security) = (0, None);
        input.fields(
            text.as_bytes(),
            &["database_security_format", "security"],
            [3, 3],
            true,
            |field, bytes| {
                if field == 0 {
                    version = input.unsigned(bytes)?.try_into().map_err(|_| invalid())?;
                } else {
                    security = Some(parts.take(input.database(bytes)?));
                }
                Ok(())
            },
        )?;
        Ok(parts.finish(Self {
            database_security_format: version,
            security: security.expect("required database authority"),
        })?)
    }
}

impl Input<'_> {
    fn database(&self, bytes: &[u8]) -> Result<Produced<BoundDatabaseSecurity>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = BoundDatabaseSecurity {
            role_owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
            acl: None,
        };
        self.fields(
            bytes,
            &["role_owner", "acl"],
            [1, 3],
            false,
            |field, bytes| {
                if field == 0 {
                    value.role_owner = self.role(bytes)?;
                } else {
                    value.acl = parts.take(self.acl(bytes, Self::database_privileges)?);
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn database_privileges(&self, bytes: &[u8]) -> Result<DatabasePrivileges, InputError> {
        let mut value = DatabasePrivileges::default();
        self.fields(
            bytes,
            &["connect", "create", "temporary"],
            [7, 7],
            false,
            |field, bytes| {
                let boolean = self.boolean(bytes)?;
                match field {
                    0 => value.connect = boolean,
                    1 => value.create = boolean,
                    _ => value.temporary = boolean,
                }
                Ok(())
            },
        )?;
        Ok(value)
    }
}

/// Move the complete decoded ACL into the native immutable owner and select a separate allowance for later edits.
pub fn adopt(
    value: Budgeted<BoundDatabaseSecurity>,
    mutations: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<DatabaseAuthority, SQLError> {
    Ok(DatabaseAuthority::from_image_value(value, cancellation)?.into_mutation_budget(mutations))
}

#[cfg(test)]
mod tests;
