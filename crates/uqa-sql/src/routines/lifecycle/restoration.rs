//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate durable routine names, object identities, signatures, and serialized dispatches.

use super::routine_signature_label;
use crate::{
    ast::{CreateFunction, FunctionBody},
    routines::routine_signature_types,
};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::RelationIdentity;

pub fn persisted_registry_relation(stored_name: &str) -> Result<RelationIdentity, String> {
    RelationIdentity::from_legacy_name(stored_name)
        .map_err(|error| format!("invalid persisted routine registry key `{stored_name}`: {error}"))
}
pub fn validate_persisted_routine_schema(
    stored_name: &str,
    relation: &RelationIdentity,
    exists: bool,
) -> Result<(), String> {
    if !exists {
        return Err(format!(
            "persisted routine `{stored_name}` references missing schema `{}`",
            relation.schema
        ));
    }
    Ok(())
}

#[derive(Default)]
pub struct RoutineRestoreBuilder {
    definitions: BTreeMap<String, Vec<CreateFunction>>,
    object_ids: BTreeSet<[u8; 16]>,
}
impl RoutineRestoreBuilder {
    pub fn insert(
        &mut self,
        stored_name: &str,
        stored_relation: &RelationIdentity,
        mut def: CreateFunction,
    ) -> Result<bool, String> {
        let object_id = def
            .object_id
            .ok_or_else(|| format!("persisted routine `{stored_name}` has no object identity"))?;
        if !self.object_ids.insert(object_id) {
            return Err(format!(
                "duplicate persisted routine object identity for `{stored_name}`"
            ));
        }
        let mut migrated = false;
        for parameter in &mut def.params {
            if let Some(default) = &mut parameter.default {
                migrated |= default.upgrade_legacy_serialized_dispatches();
            }
        }
        if let FunctionBody::Statements(statements) = &mut def.body {
            for statement in statements {
                migrated |= statement.upgrade_legacy_serialized_dispatches();
            }
        }
        let definition_relation =
            RelationIdentity::from_legacy_name(&def.name).map_err(|error| {
                format!(
                    "invalid persisted routine definition name `{}`: {error}",
                    def.name
                )
            })?;
        if &definition_relation != stored_relation {
            return Err(format!(
                "persisted routine registry key `{stored_name}` does not match definition `{}`",
                def.name
            ));
        }
        let canonical_name = stored_relation.qualified_name();
        def.name.clone_from(&canonical_name);
        let signature = routine_signature_types(&def);
        let definitions = self.definitions.entry(canonical_name.clone()).or_default();
        if definitions
            .iter()
            .any(|existing| routine_signature_types(existing) == signature)
        {
            return Err(format!(
                "duplicate persisted routine identity `{}`",
                routine_signature_label(&canonical_name, &signature)
            ));
        }
        definitions.push(def);
        Ok(migrated)
    }
    pub fn into_definitions(self) -> BTreeMap<String, Vec<CreateFunction>> {
        self.definitions
    }
}
