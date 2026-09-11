//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Restore the complete routine namespace before compiling bodies, rolling back failed publication.

use super::{
    catalog::{RoutineRegistryPublication, RoutineRegistryState, FUNCTIONS_METADATA_KEY},
    definition::{compile_catalog_bound_routine, RoutineDefinitionContext},
};
use std::{collections::BTreeMap, sync::Arc};
use uqa_sql::{
    ast::CreateFunction,
    routines::{
        dependencies::RoutineCompilationMode,
        lifecycle::{restoration as analysis, RoutineRegistry},
        routine_signature_types, CompiledFunctionBody, SQLUserFunction,
    },
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

pub struct PendingSQLFunctionRestore {
    definitions: BTreeMap<String, Vec<CreateFunction>>,
    migrated: bool,
    previous: RoutineRegistry,
}
pub trait RoutineRestoreSchemas {
    fn routine_schema_exists(&self, schema: &str) -> bool;
}
pub struct RoutineRestoreContext<'a> {
    pub registry: &'a dyn RoutineRegistryState,
    pub publication: &'a dyn RoutineRegistryPublication,
    pub schemas: &'a dyn RoutineRestoreSchemas,
    pub definition: RoutineDefinitionContext<'a>,
}

fn canonicalize_persisted_sql_functions(
    schemas: &dyn RoutineRestoreSchemas,
    definitions: BTreeMap<String, Vec<CreateFunction>>,
) -> StorageBackendResult<(BTreeMap<String, Vec<CreateFunction>>, bool)> {
    let mut builder = analysis::RoutineRestoreBuilder::default();
    let mut migrated = false;
    for (stored_name, overloads) in definitions {
        let stored_relation = analysis::persisted_registry_relation(&stored_name)
            .map_err(StorageBackendError::Other)?;
        analysis::validate_persisted_routine_schema(
            &stored_name,
            &stored_relation,
            schemas.routine_schema_exists(&stored_relation.schema),
        )
        .map_err(StorageBackendError::Other)?;
        for mut def in overloads {
            if def.object_id.is_none() || def.object_id == Some([0; 16]) {
                def.object_id = Some(crate::catalog::identity::new_nonzero_catalog_identity(
                    "routine",
                    "object identity",
                )?);
                migrated = true;
            }
            migrated |= builder
                .insert(&stored_name, &stored_relation, def)
                .map_err(StorageBackendError::Other)?;
        }
    }
    Ok((builder.into_definitions(), migrated))
}

pub fn install_sql_function_restore_placeholders(
    context: &RoutineRestoreContext<'_>,
    catalog: &dyn CatalogFacade,
    allows_migration: bool,
) -> StorageBackendResult<Option<PendingSQLFunctionRestore>> {
    let Some(json) = catalog.get_metadata(FUNCTIONS_METADATA_KEY)? else {
        return Ok(None);
    };
    let defs = serde_json::from_str::<BTreeMap<String, Vec<CreateFunction>>>(&json)?;
    let (canonical_defs, migrated) = canonicalize_persisted_sql_functions(context.schemas, defs)?;
    if migrated && !allows_migration {
        return Err(StorageBackendError::Other(
            "routine catalog requires an initial-open object-identity migration".into(),
        ));
    }

    // Install definition-only placeholders before compiling stored SQL-standard bodies so every exact routine identity is visible while durable function bindings are rebuilt. A compilation failure restores the previous registry atomically.
    let placeholders = canonical_defs
        .iter()
        .map(|(name, definitions)| {
            let mut overloads = definitions
                .iter()
                .cloned()
                .map(|def| {
                    Arc::new(SQLUserFunction {
                        def,
                        compiled: CompiledFunctionBody::SQL(Vec::new()),
                    })
                })
                .collect::<Vec<_>>();
            overloads.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
            (name.clone(), overloads)
        })
        .collect();
    let previous = std::mem::replace(&mut **context.registry.routines_write(), placeholders);
    Ok(Some(PendingSQLFunctionRestore {
        definitions: canonical_defs,
        migrated,
        previous,
    }))
}

pub fn finalize_sql_function_restore(
    context: &RoutineRestoreContext<'_>,
    pending: PendingSQLFunctionRestore,
    allows_migration: bool,
) -> StorageBackendResult<()> {
    let PendingSQLFunctionRestore {
        definitions,
        mut migrated,
        previous,
    } = pending;
    let compiled = (|| {
        let mut restored: BTreeMap<String, Vec<Arc<SQLUserFunction>>> = BTreeMap::new();
        for (name, definitions) in definitions {
            let mut overloads = Vec::with_capacity(definitions.len());
            for mut def in definitions {
                let (compiled, definition_migrated) = compile_catalog_bound_routine(
                    &context.definition,
                    &mut def,
                    RoutineCompilationMode::Persisted,
                )
                .map_err(|err| StorageBackendError::Other(err.to_string()))?;
                migrated |= definition_migrated;
                overloads.push(Arc::new(SQLUserFunction { def, compiled }));
            }
            overloads.sort_by(|left, right| {
                routine_signature_types(&left.def)
                    .cmp(&routine_signature_types(&right.def))
                    .then_with(|| left.def.is_procedure.cmp(&right.def.is_procedure))
            });
            restored.insert(name, overloads);
        }
        Ok(restored)
    })();
    let restored = match compiled {
        Ok(restored) => restored,
        Err(error) => {
            **context.registry.routines_write() = previous;
            return Err(error);
        }
    };
    if migrated && !allows_migration {
        **context.registry.routines_write() = previous;
        return Err(StorageBackendError::Other(
            "routine-owned dependency bindings require an initial-open object-identity migration"
                .into(),
        ));
    }
    if migrated {
        if let Err(error) = context.publication.persist_routine_definitions(&restored) {
            **context.registry.routines_write() = previous;
            return Err(StorageBackendError::Other(error.to_string()));
        }
    }
    **context.registry.routines_write() = restored;
    Ok(())
}
