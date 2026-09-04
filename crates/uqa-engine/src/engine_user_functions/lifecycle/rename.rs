//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Routine rename planning and durable dependency rewrites.

use std::collections::BTreeMap;

use uqa_sql::ast::{FunctionBinding, FunctionBody, RenameRoutineStmt};
use uqa_sql::SQLError;

use crate::{
    engine_roles::role_inherits, engine_schema_security::SchemaAclPrivilege, Arc, Engine,
    RelationIdentity,
};

use super::super::declaration::resolve_routine_identity_types;
use super::super::resolution::routine_signature_types;
use super::super::SQLUserFunction;

struct RoutineRenameTarget {
    old_name: String,
    new_name: String,
    position: usize,
    binding: FunctionBinding,
}

impl Engine {
    pub(crate) fn rename_sql_routine(&self, stmt: &RenameRoutineStmt) -> Result<(), SQLError> {
        self.with_implicit_transaction(|engine| engine.rename_sql_routine_inner(stmt))
    }

    fn rename_sql_routine_inner(&self, stmt: &RenameRoutineStmt) -> Result<(), SQLError> {
        self.prepare_explicit_transaction_writer()?;
        self.synchronize_catalog_registries().map_err(|error| {
            SQLError::Internal(format!(
                "synchronize catalogs before routine rename: {error}"
            ))
        })?;
        let registry = self.durable.sql_user_functions.read().clone();
        let target = self.resolve_routine_rename_target(stmt, &registry)?;
        let renamed_registry = Self::move_routine_registry_entry(registry, &target)?;
        *self.durable.sql_user_functions.write() = renamed_registry;

        let rewritten_registry =
            self.rewrite_routine_owned_dependency_identity(&target.binding, &target.new_name)?;
        *self.durable.sql_user_functions.write() = rewritten_registry.clone();
        self.rewrite_generated_routine_identity(&target.binding, &target.new_name)
            .map_err(|error| {
                SQLError::Internal(format!("rewrite generated routine dependencies: {error}"))
            })?;
        self.rewrite_view_routine_identity(&target.binding, &target.new_name)
            .map_err(|error| {
                SQLError::Internal(format!("rewrite view routine dependencies: {error}"))
            })?;
        self.rewrite_event_routine_identity(&target.binding, &target.new_name)?;
        self.persist_sql_functions_snapshot(&rewritten_registry)?;
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn resolve_routine_rename_target(
        &self,
        stmt: &RenameRoutineStmt,
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    ) -> Result<RoutineRenameTarget, SQLError> {
        let requested_types = resolve_routine_identity_types(
            self,
            stmt.arg_types.as_deref(),
            &stmt.arg_type_references,
            "ALTER routine RENAME",
        )?;
        let (old_name, position) = self.resolve_sql_routine_alter_target(
            registry,
            &stmt.name,
            requested_types.as_deref(),
            stmt.kind,
        )?;
        let function = registry
            .get(&old_name)
            .and_then(|overloads| overloads.get(position))
            .ok_or_else(|| {
                SQLError::Internal(format!(
                    "resolved ALTER routine target `{old_name}` disappeared before rename"
                ))
            })?;
        let current_user = self.current_user_name();
        let roles = self.durable.roles.read();
        let memberships = self.durable.role_memberships.read();
        Self::ensure_routine_owner_as(
            &function.def,
            role_inherits(&roles, &memberships, &current_user, &function.def.owner),
        )?;
        drop(memberships);
        drop(roles);
        let old_identity = RelationIdentity::from_legacy_name(&old_name).map_err(|error| {
            SQLError::Internal(format!("decode routine identity `{old_name}`: {error}"))
        })?;
        self.require_schema_privilege(
            &old_identity.schema,
            &current_user,
            SchemaAclPrivilege::Create,
        )?;
        let signature = routine_signature_types(&function.def);
        let new_name = Self::routine_rename_destination(stmt, &old_identity, &signature, registry)?;
        let object_id = function.def.object_id.ok_or_else(|| {
            SQLError::Internal(format!(
                "routine `{old_name}` has no catalog object identity"
            ))
        })?;
        Ok(RoutineRenameTarget {
            old_name: old_name.clone(),
            new_name,
            position,
            binding: FunctionBinding {
                object_id: Some(object_id),
                name: old_name,
                argument_types: signature,
                builtin: false,
                dispatch: None,
                invocation: None,
                resolution_error: None,
            },
        })
    }

    fn routine_rename_destination(
        stmt: &RenameRoutineStmt,
        old_identity: &RelationIdentity,
        signature: &[String],
        registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    ) -> Result<String, SQLError> {
        let (new_schema, new_local_name) = RelationIdentity::parse_reference(&stmt.new_name)
            .map_err(|error| SQLError::Routine {
                sqlstate: "42602".into(),
                message: format!("invalid routine name `{}`: {error}", stmt.new_name),
            })?;
        if new_schema.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "ALTER ROUTINE RENAME TO requires an unqualified new name".into(),
            });
        }
        let new_name = RelationIdentity::new(&old_identity.schema, new_local_name).qualified_name();
        if registry.get(&new_name).is_some_and(|overloads| {
            overloads
                .iter()
                .any(|function| routine_signature_types(&function.def) == signature)
        }) {
            return Err(SQLError::Routine {
                sqlstate: "42723".into(),
                message: format!(
                    "function \"{}\" already exists with same argument types",
                    RelationIdentity::from_legacy_name(&new_name)
                        .map_or_else(|_| new_name.clone(), |identity| identity.name)
                ),
            });
        }
        Ok(new_name)
    }

    fn move_routine_registry_entry(
        mut registry: BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
        target: &RoutineRenameTarget,
    ) -> Result<BTreeMap<String, Vec<Arc<SQLUserFunction>>>, SQLError> {
        let old_overloads = registry.get_mut(&target.old_name).ok_or_else(|| {
            SQLError::Internal(format!(
                "resolved ALTER routine registry entry `{}` disappeared before rename",
                target.old_name
            ))
        })?;
        let renamed = old_overloads.remove(target.position);
        if old_overloads.is_empty() {
            registry.remove(&target.old_name);
        }
        let mut renamed_definition = renamed.def.clone();
        renamed_definition.name.clone_from(&target.new_name);
        let new_overloads = registry.entry(target.new_name.clone()).or_default();
        new_overloads.push(Arc::new(SQLUserFunction {
            def: renamed_definition,
            compiled: renamed.compiled.clone(),
        }));
        new_overloads.sort_by(|left, right| {
            routine_signature_types(&left.def)
                .cmp(&routine_signature_types(&right.def))
                .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
        });
        Ok(registry)
    }

    fn rewrite_routine_owned_dependency_identity(
        &self,
        target: &FunctionBinding,
        new_name: &str,
    ) -> Result<BTreeMap<String, Vec<Arc<SQLUserFunction>>>, SQLError> {
        let registry = self.durable.sql_user_functions.read().clone();
        let mut rewritten = BTreeMap::new();
        for (name, overloads) in registry {
            let mut next_overloads = Vec::with_capacity(overloads.len());
            for function in overloads {
                let mut def = function.def.clone();
                let mut changed = false;
                for default in def
                    .params
                    .iter_mut()
                    .filter_map(|parameter| parameter.default.as_mut())
                {
                    changed |= crate::engine_events::rewrite_expression_routine_identity(
                        default, target, new_name,
                    )?;
                }
                if let FunctionBody::Statements(statements) = &mut def.body {
                    for statement in statements {
                        changed |= crate::engine_events::rewrite_statement_routine_identity(
                            statement, target, new_name,
                        )?;
                    }
                }
                if changed {
                    let compiled = self.compile_persisted_sql_function(&def)?;
                    next_overloads.push(Arc::new(SQLUserFunction { def, compiled }));
                } else {
                    next_overloads.push(function);
                }
            }
            rewritten.insert(name, next_overloads);
        }
        Ok(rewritten)
    }
}
