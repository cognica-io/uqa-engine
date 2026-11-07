//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema and database authority preserve optional ACLs, grantor paths and tuple incarnations.

use super::{invalid, Input};
use crate::{
    catalog::security::{
        database::{BoundDatabaseSecurity, DatabaseAuthority, DatabasePrivileges},
        schema_registry::SchemaRegistry,
        BoundSchemaSecurity,
    },
    SQLError,
};
use std::io::Read;
use uqa_core::{
    catalog_schema::{SchemaPrivileges, SchemaTupleIdentity},
    memory::{Budgeted, BudgetedMap, MemoryBudget},
    CancellationToken,
};

pub(in super::super) fn schemas(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<SchemaRegistry, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    let count = input.header(super::super::SCHEMAS)?;
    let mut result = SchemaRegistry::with_budget(memory);
    let mut oids = BudgetedMap::new(memory);
    let mut identities = BudgetedMap::new(memory);
    for _ in 0..count {
        cancellation.check()?;
        let name = input.ordered_name(result.last_durable_key())?;
        if crate::catalog::temporary_namespace::is_temporary_schema_name(&name) {
            return Err(invalid("temporary schema"));
        }
        let tuple = if input.flag()? {
            let tuple = SchemaTupleIdentity {
                oid: input.oid()?,
                object_id: input.binary.array()?,
                revision: input.binary.array()?,
            };
            if !tuple.is_valid() {
                return Err(invalid("schema tuple"));
            }
            if oids.insert(tuple.oid, ())?.is_some()
                || identities.insert(tuple.object_id, ())?.is_some()
            {
                return Err(invalid("duplicate schema identity"));
            }
            Some(tuple)
        } else {
            None
        };
        let role_owner = input.identity()?;
        let (acl, reservation) = input.acl::<SchemaPrivileges>()?.into_parts();
        result.insert_image_entry(
            name,
            Budgeted::new(
                BoundSchemaSecurity {
                    tuple,
                    role_owner,
                    acl,
                },
                reservation,
            ),
        )?;
    }
    input.binary.end()?;
    Ok(result)
}

pub(in super::super) fn database(
    reader: &mut dyn Read,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> Result<DatabaseAuthority, SQLError> {
    let mut input = Input::new(reader, memory, cancellation);
    if input.header(super::super::DATABASE)? != 1 {
        return Err(invalid("database authority count"));
    }
    let role_owner = input.identity()?;
    let (acl, reservation) = input.acl::<DatabasePrivileges>()?.into_parts();
    let value = Budgeted::new(BoundDatabaseSecurity { role_owner, acl }, reservation);
    input.binary.end()?;
    DatabaseAuthority::from_image_value(value, cancellation)
}
