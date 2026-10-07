//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-wrapper declaration publication inside the existing metadata transaction.

use super::ForeignCreationContext;
use crate::row_locks::{shared_objects::SharedCatalogLock, RelationLockMode};
use uqa_sql::{
    ast::CreateForeignWrapper,
    catalog::{
        dependencies::FOREIGN_WRAPPER_CLASS,
        foreign_wrapper::{
            ForeignWrapperDefinition, ForeignWrapperFunction, ForeignWrapperHandler,
            ForeignWrapperReference,
        },
    },
    schema::foreign_wrappers as analysis,
    SQLError,
};

impl ForeignCreationContext<'_> {
    pub fn register_foreign_wrapper_statement(
        &self,
        statement: &CreateForeignWrapper,
    ) -> Result<(), SQLError> {
        self.namespace
            .synchronize_catalog_registries()
            .map_err(storage_error)?;
        analysis::ensure_create_authority(
            &statement.name,
            &self.creation.names.current_role(),
            &self.creation.roles.role_definitions(),
        )?;
        if self.registry.wrappers().contains_key(&statement.name) {
            return Err(analysis::duplicate_wrapper(&statement.name));
        }
        let owner = self.creation.bind_owner()?;
        let oid = crate::catalog::identity::reserve_new_catalog_oid(
            self.creation.locks,
            FOREIGN_WRAPPER_CLASS,
            "foreign-data wrapper",
            |oid| {
                Ok(self
                    .registry
                    .wrappers()
                    .values()
                    .any(|wrapper| i64::from(wrapper.identity.oid) == oid))
            },
        )?;
        let functions =
            analysis::bind_functions(&statement.functions, |name, arguments, display| {
                self.lookup_wrapper_function(name, arguments, display)
            })?;
        let options = analysis::creation_options(&statement.options)?;
        if let Some(function) = &functions.validator {
            self.invoke_foreign_validator(
                &ForeignWrapperFunction {
                    oid: function.oid,
                    binding: function.binding.clone(),
                },
                &options,
                FOREIGN_WRAPPER_CLASS,
            )?;
        }
        self.creation.retain_owner(&owner)?;
        let guard = self.creation.locks.acquire_shared_catalog(
            SharedCatalogLock::Name {
                class_id: FOREIGN_WRAPPER_CLASS,
                name: &statement.name,
            },
            RelationLockMode::AccessExclusive,
        )?;
        self.creation.locks.refresh_shared_catalog()?;
        if self.registry.wrappers().contains_key(&statement.name) {
            return Err(SQLError::Diagnostic { sqlstate: "23505".into(), message: "duplicate key value violates unique constraint \"pg_foreign_data_wrapper_name_index\"".into(), detail: Some(format!("Key (fdwname)=({}) already exists.", statement.name)), hint: None });
        }
        guard.retain();
        self.creation
            .runtime
            .fence_catalog_writer_and_refresh_snapshot()?;
        owner.revalidate(&self.creation.roles.role_definitions())?;
        let definition = ForeignWrapperDefinition {
            name: statement.name.clone(),
            identity: ForeignWrapperReference {
                oid: u32::try_from(oid).map_err(|e| SQLError::Internal(e.to_string()))?,
                object_id: crate::catalog::identity::new_nonzero_catalog_identity(
                    &statement.name,
                    "foreign-data wrapper",
                )
                .map_err(storage_error)?,
            },
            owner: owner.identity(),
            handler: functions
                .handler
                .map_or(ForeignWrapperHandler::None, |function| {
                    ForeignWrapperHandler::Function(ForeignWrapperFunction {
                        oid: function.oid,
                        binding: function.binding,
                    })
                }),
            validator: functions.validator.map(|function| ForeignWrapperFunction {
                oid: function.oid,
                binding: function.binding,
            }),
            options,
        };
        if let Some(catalog) = self.catalog {
            crate::catalog::foreign::wrappers::persist(catalog, &definition)
                .map_err(storage_error)?;
        }
        self.registry
            .wrappers_write()
            .insert(statement.name.clone(), definition);
        self.changes.catalog_registry_changed();
        Ok(())
    }

    pub(super) fn invoke_foreign_validator(
        &self,
        function: &ForeignWrapperFunction,
        options: &[(String, String)],
        class_id: u32,
    ) -> Result<(), SQLError> {
        let values = options
            .iter()
            .map(|(name, value)| uqa_core::Value::Str(format!("{name}={value}")))
            .collect();
        let array = uqa_core::ArrayValue::try_new(values)
            .ok_or_else(|| SQLError::Internal("invalid foreign option array".into()))?;
        crate::routines::invocation::call_catalog_validator(
            &self.invocation,
            function,
            &[
                uqa_core::Value::Array(array),
                uqa_core::Value::Int(i64::from(class_id)),
            ],
        )
    }

    pub(super) fn validate_foreign_table_options(
        &self,
        statement: &uqa_sql::ast::CreateForeignTable,
        sql_options: bool,
    ) -> Result<(), SQLError> {
        let options = if sql_options {
            analysis::creation_options(&statement.options)?
        } else {
            // The host API accepts map options with arbitrary names and last-key wins.
            statement
                .options
                .iter()
                .cloned()
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_iter()
                .collect()
        };
        let validator = {
            let servers = self.registry.servers();
            let server = servers.get(&statement.server_name).ok_or_else(|| {
                uqa_sql::schema::foreign_servers::missing_server(&statement.server_name)
            })?;
            server
                .bound_wrapper(&self.registry.wrappers())?
                .validator
                .clone()
        };
        if let Some(validator) = validator {
            self.invoke_foreign_validator(&validator, &options, 3118)?;
        }
        Ok(())
    }
}

fn storage_error(error: uqa_storage::StorageBackendError) -> SQLError {
    SQLError::Internal(format!("foreign-wrapper catalog: {error}"))
}
