//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored relation and built-in routine authority retain every ACL tuple, including empty ones.

use crate::{
    ast::ObjectAclEntry,
    catalog::security::{
        builtin_routines::{BuiltinRoutineSecurities, BuiltinRoutineSecurity},
        system_relations::{SystemAcl, SystemRelationSecurities, SystemRelationSecurity},
        table_binding::CapturedTableSecurity,
        BoundTableSecurity,
    },
    plan::copy::SyntaxProduction,
    SQLError,
};
use std::collections::BTreeMap;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

#[derive(Debug, PartialEq, Eq)]
pub struct CapturedSystemRelationSecurity {
    pub table: Option<SystemAcl>,
    pub columns: Vec<(String, SystemAcl)>,
}

fn relation(
    value: &RelationIdentity,
    output: &SyntaxProduction<'_>,
) -> Result<RelationIdentity, SQLError> {
    let RelationIdentity { schema, name } = value;
    Ok(RelationIdentity {
        schema: output.text(schema)?,
        name: output.text(name)?,
    })
}

impl BuiltinRoutineSecurity {
    pub fn capture_security(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let Self {
            revision,
            execute_acl,
        } = self;
        let output = SyntaxProduction::new(*control);
        let mut copied = output.vector(execute_acl.len())?;
        for ObjectAclEntry {
            role,
            grantor,
            grant_option,
        } in execute_acl
        {
            copied.push(ObjectAclEntry {
                role: *role,
                grantor: *grantor,
                grant_option: *grant_option,
            })?;
        }
        let copied = Self {
            revision: *revision,
            execute_acl: copied.finish()?,
        };
        output.finish(copied)
    }
}

fn system_acl(value: &SystemAcl, output: &SyntaxProduction<'_>) -> Result<SystemAcl, SQLError> {
    let SystemAcl { revision, acl } = value;
    Ok(SystemAcl {
        revision: *revision,
        acl: super::acl_entries(acl, output)?,
    })
}

impl SystemRelationSecurity {
    pub fn capture_security(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedSystemRelationSecurity>, SQLError> {
        let Self { table, columns } = self;
        let output = SyntaxProduction::new(*control);
        let table = table
            .as_ref()
            .map(|table| system_acl(table, &output))
            .transpose()?;
        let mut copied_columns = output.vector(columns.len())?;
        for (name, value) in columns {
            copied_columns.push((output.text(name)?, system_acl(value, &output)?))?;
        }
        let copied = CapturedSystemRelationSecurity {
            table,
            columns: copied_columns.finish()?,
        };
        output.finish(copied)
    }
}

pub fn builtin_routines(
    values: &BuiltinRoutineSecurities,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(u32, BuiltinRoutineSecurity)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut copied = output.vector(values.len())?;
    for (oid, value) in values {
        let value = output.take(value.capture_security(control)?);
        copied.push((*oid, value))?;
    }
    let copied = copied.finish()?;
    output.finish(copied)
}

pub fn system_relations(
    values: &SystemRelationSecurities,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(RelationIdentity, CapturedSystemRelationSecurity)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut copied = output.vector(values.len())?;
    for (name, value) in values {
        let name = relation(name, &output)?;
        let value = output.take(value.capture_security(control)?);
        copied.push((name, value))?;
    }
    let copied = copied.finish()?;
    output.finish(copied)
}

pub fn tables(
    values: &BTreeMap<RelationIdentity, BoundTableSecurity>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(RelationIdentity, CapturedTableSecurity)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut copied = output.vector(values.len())?;
    for (name, value) in values {
        let name = relation(name, &output)?;
        let value = output.take(value.capture_security(control)?);
        copied.push((name, value))?;
    }
    let copied = copied.finish()?;
    output.finish(copied)
}

#[cfg(test)]
mod tests;
