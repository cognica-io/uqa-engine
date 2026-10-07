//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Create server identities and retain their owning role through catalog publication.

use super::ForeignCreationContext;
use crate::row_locks::{shared_objects::SharedCatalogLock, RelationLockMode};
use uqa_sql::{
    ast::CreateForeignServer,
    catalog::{
        dependencies::FOREIGN_SERVER_CLASS,
        foreign_server::{ForeignServerDefinition, ForeignServerMetadata},
    },
    SQLError, SQLNotice,
};

impl ForeignCreationContext<'_> {
    pub fn register_foreign_server_inner(
        &self,
        name: String,
        fdw_type: &str,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), String> {
        self.create_server(
            &CreateForeignServer {
                name,
                fdw_type: fdw_type.to_owned(),
                options: options
                    .into_iter()
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
                if_not_exists,
                server_type: None,
                version: None,
            },
            |options| Ok(options.iter().cloned().collect()),
        )
        .map_err(|error| error.to_string())
    }

    pub fn register_foreign_server_statement(
        &self,
        statement: &CreateForeignServer,
    ) -> Result<(), SQLError> {
        self.create_server(
            statement,
            uqa_sql::schema::foreign_servers::creation_options,
        )
    }

    // The Rust API takes map options (including arbitrary keys and last-key wins); SQL validates its written declarations at the PostgreSQL boundary.
    fn create_server(
        &self,
        statement: &CreateForeignServer,
        options: impl FnOnce(
            &[(String, String)],
        ) -> Result<std::collections::BTreeMap<String, String>, SQLError>,
    ) -> Result<(), SQLError> {
        uqa_sql::catalog::foreign_server::validate_foreign_server_name(&statement.name)?;
        let owner = self.creation.bind_owner()?;
        self.namespace
            .synchronize_catalog_registries()
            .map_err(storage_error)?;
        if self.registry.servers().contains_key(&statement.name) {
            let message = format!("server \"{}\" already exists", statement.name);
            if statement.if_not_exists {
                self.notices
                    .push(SQLNotice::notice(format!("{message}, skipping")).with_sqlstate("42710"));
                return Ok(());
            }
            return Err(diagnostic("42710", message));
        }
        let wrapper = self.bind_server_wrapper(&statement.fdw_type)?;
        self.creation.retain_owner(&owner)?;
        let oid = crate::catalog::identity::reserve_new_catalog_oid(
            self.creation.locks,
            FOREIGN_SERVER_CLASS,
            "foreign server",
            |oid| {
                Ok(self
                    .registry
                    .servers()
                    .values()
                    .any(|server| i64::from(server.metadata.oid) == oid))
            },
        )?;
        let options = options(&statement.options)?;
        if let Some(validator) = &wrapper.validator {
            self.invoke_foreign_validator(validator, &statement.options, FOREIGN_SERVER_CLASS)?;
        }
        let guard = self.creation.locks.acquire_shared_catalog(
            SharedCatalogLock::Name {
                class_id: FOREIGN_SERVER_CLASS,
                name: &statement.name,
            },
            RelationLockMode::AccessExclusive,
        )?;
        self.creation.locks.refresh_shared_catalog()?;
        if self.registry.servers().contains_key(&statement.name) {
            return Err(SQLError::Diagnostic {
                sqlstate: "23505".into(),
                message: "duplicate key value violates unique constraint \"pg_foreign_server_name_index\"".into(),
                detail: Some(format!("Key (srvname)=({}) already exists.", statement.name)),
                hint: None,
            });
        }
        guard.retain();
        self.creation
            .runtime
            .fence_catalog_writer_and_refresh_snapshot()?;
        owner.revalidate(&self.creation.roles.role_definitions())?;
        let definition = ForeignServerDefinition {
            name: statement.name.clone(),
            fdw_type: statement.fdw_type.clone(),
            options,
            metadata: ForeignServerMetadata {
                option_order: Some(
                    statement
                        .options
                        .iter()
                        .map(|(name, _)| name.clone())
                        .collect(),
                ),
                wrapper_reference: Some(wrapper.identity),
                oid: u32::try_from(oid).map_err(|error| SQLError::Internal(error.to_string()))?,
                object_id: crate::catalog::identity::new_nonzero_catalog_identity(
                    &statement.name,
                    "foreign server",
                )
                .map_err(storage_error)?,
                owner: owner.identity(),
                server_type: statement.server_type.clone(),
                version: statement.version.clone(),
            },
        };
        if let Some(catalog) = self.catalog {
            crate::catalog::foreign::servers::persist(catalog, &definition)
                .map_err(storage_error)?;
        }
        self.registry
            .servers_write()
            .insert(statement.name.clone(), definition);
        self.changes.catalog_registry_changed();
        Ok(())
    }

    fn bind_server_wrapper(
        &self,
        name: &str,
    ) -> Result<uqa_sql::catalog::foreign_wrapper::ForeignWrapperDefinition, SQLError> {
        let wrapper = self.registry.wrappers().get(name).cloned().ok_or_else(|| {
            diagnostic(
                "42704",
                format!("foreign-data wrapper \"{name}\" does not exist"),
            )
        })?;
        uqa_sql::schema::foreign_wrappers::ensure_wrapper_usage(
            &wrapper,
            &self.creation.names.current_role(),
            self.creation.roles,
        )?;
        // GetForeignDataWrapperByName captures the reference without retaining an object lock.
        Ok(wrapper)
    }
}

fn diagnostic(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

fn storage_error(error: uqa_storage::StorageBackendError) -> SQLError {
    SQLError::Internal(format!("foreign server catalog: {error}"))
}
