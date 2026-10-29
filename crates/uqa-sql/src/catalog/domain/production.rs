//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A selected domain descriptor owns its exact copied declaration and ACL allocations.

use super::StoredDomain;
use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    RelationIdentity,
};

impl StoredDomain {
    pub fn clone_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Self>, SQLError> {
        let schema = control.copy_text(&self.identity.schema)?;
        let name = control.copy_text(&self.identity.name)?;
        let definition = crate::plan::copy::copy_syntax_with_control(&self.definition, control)?;
        let array_name = self
            .array_name
            .as_deref()
            .map(|name| control.copy_text(name))
            .transpose()?;
        let usage_acl = self
            .usage_acl
            .as_ref()
            .map(|acl| {
                let mut entries = ProductionVec::new(*control);
                entries.reserve(acl.len())?;
                for entry in acl {
                    entries.push_produced(
                        control.finish(entry.clone(), control.empty_reservation())?,
                    )?;
                }
                entries.finish()
            })
            .transpose()?;
        let (schema, mut memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        memory = control.combine(memory, name_memory);
        let (definition, definition_memory) = definition.into_parts();
        memory = control.combine(memory, definition_memory);
        let array_name = array_name.map(|name| {
            let (name, name_memory) = name.into_parts();
            memory = control.combine(memory.take(), name_memory);
            name
        });
        let usage_acl = usage_acl.map(|acl| {
            let (acl, acl_memory) = acl.into_parts();
            memory = control.combine(memory.take(), acl_memory);
            acl
        });
        Ok(control.finish(
            Self {
                object_id: self.object_id,
                oid: self.oid,
                array_oid: self.array_oid,
                identity: RelationIdentity { schema, name },
                owner: self.owner,
                definition,
                array_name,
                usage_acl,
            },
            memory,
        )?)
    }
}

#[cfg(test)]
mod tests;
