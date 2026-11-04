//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Detached catalog authority retains bound role incarnations and complete namespace identities.

use super::{database::binding::BoundDatabaseSecurity, BoundSchemaSecurity, BoundSequenceSecurity};
use crate::{plan::copy::SyntaxProduction, SQLError};
use std::collections::BTreeMap;
use uqa_core::{
    catalog_role::BoundAclEntry,
    memory::{Produced, ProductionControl},
    RelationIdentity,
};

pub mod relations;

fn acl_entries<P: Copy>(
    entries: &[BoundAclEntry<P>],
    output: &SyntaxProduction<'_>,
) -> Result<Vec<BoundAclEntry<P>>, SQLError> {
    let mut copied = output.vector(entries.len())?;
    for BoundAclEntry {
        role,
        grantor,
        privileges,
        grant_options,
    } in entries
    {
        output.check()?;
        copied.push(BoundAclEntry {
            role: *role,
            grantor: *grantor,
            privileges: *privileges,
            grant_options: *grant_options,
        })?;
    }
    copied.finish()
}

fn acl<P: Copy>(
    entries: Option<&[BoundAclEntry<P>]>,
    output: &SyntaxProduction<'_>,
) -> Result<Option<Vec<BoundAclEntry<P>>>, SQLError> {
    entries
        .map(|entries| acl_entries(entries, output))
        .transpose()
}

impl BoundDatabaseSecurity {
    pub fn capture_security(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let Self {
            role_owner,
            acl: entries,
        } = self;
        let output = SyntaxProduction::new(*control);
        let copied = Self {
            role_owner: *role_owner,
            acl: acl(entries.as_deref(), &output)?,
        };
        output.finish(copied)
    }
}

impl BoundSchemaSecurity {
    pub fn capture_security(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let Self {
            tuple,
            role_owner,
            acl: entries,
        } = self;
        let output = SyntaxProduction::new(*control);
        let copied = Self {
            tuple: *tuple,
            role_owner: *role_owner,
            acl: acl(entries.as_deref(), &output)?,
        };
        output.finish(copied)
    }
}

impl BoundSequenceSecurity {
    pub fn capture_security(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let Self {
            role_owner,
            acl: entries,
        } = self;
        let output = SyntaxProduction::new(*control);
        let copied = Self {
            role_owner: *role_owner,
            acl: acl(entries.as_deref(), &output)?,
        };
        output.finish(copied)
    }
}

pub fn schemas(
    values: &BTreeMap<String, BoundSchemaSecurity>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(String, BoundSchemaSecurity)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let count = values.keys().try_fold(0, |count, name| {
        output.check()?;
        Ok::<_, SQLError>(
            count
                + usize::from(!crate::catalog::temporary_namespace::is_temporary_schema_name(name)),
        )
    })?;
    let mut copied = output.vector(count)?;
    for (name, value) in values {
        output.check()?;
        if crate::catalog::temporary_namespace::is_temporary_schema_name(name) {
            continue;
        }
        let name = output.text(name)?;
        let value = output.take(value.capture_security(control)?);
        copied.push((name, value))?;
    }
    let copied = copied.finish()?;
    output.finish(copied)
}

pub fn sequences(
    values: &BTreeMap<RelationIdentity, BoundSequenceSecurity>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(RelationIdentity, BoundSequenceSecurity)>>, SQLError> {
    let output = SyntaxProduction::new(*control);
    let mut copied = output.vector(values.len())?;
    for (name, value) in values {
        let RelationIdentity { schema, name } = name;
        let name = RelationIdentity {
            schema: output.text(schema)?,
            name: output.text(name)?,
        };
        let value = output.take(value.capture_security(control)?);
        copied.push((name, value))?;
    }
    let copied = copied.finish()?;
    output.finish(copied)
}

#[cfg(test)]
mod tests;
