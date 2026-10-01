//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind enum DDL to namespace, identity, publication and transaction visibility state.

use crate::Engine;
use std::collections::BTreeMap;
use uqa_execution::catalog::enum_type::{EnumRegistry, EnumRegistryPublication, EnumRegistryRead};
use uqa_execution::schema::enums::{EnumLabelVisibility, EnumTypeContext};
use uqa_sql::catalog::roles::RoleDefinition;
use uqa_sql::expr::enums::{EnumLabelCatalog, EnumTypeLabels};
use uqa_sql::schema::domains::removal::TypeObjectBinding;
use uqa_sql::SQLError;

impl Engine {
    pub(crate) fn enum_type_context(&self) -> EnumTypeContext<'_> {
        EnumTypeContext {
            creation: self.relation_creation_context(),
            identities: self.catalog_identity_reservation_context(),
            writer: self,
            binding: TypeObjectBinding {
                catalog: self,
                authority: self,
                session: self,
            },
            allocate_identity: || {
                crate::new_nonzero_catalog_identity("enum", "object identity")
                    .map_err(|error| SQLError::Internal(error.to_string()))
            },
            publication: self,
            domains: self,
            composites: self,
            changes: self,
            visibility: self,
            notices: self,
        }
    }
}

impl Engine {
    /// Enum definitions restore before domains and relations, whose declared types can refer to them.
    pub(crate) fn restore_enums_from_catalog(
        &self,
        catalog: &dyn uqa_storage::CatalogFacade,
    ) -> uqa_storage::StorageBackendResult<()> {
        let registry =
            uqa_execution::catalog::enum_type::restore(catalog, &self.durable.roles.read())?;
        *self.durable.enums.write() = registry;
        Ok(())
    }
}

impl Engine {
    /// The enum registry generation of the running statement: the pinned query snapshot when one exists, otherwise the session's current definitions.
    pub(crate) fn enum_registry_snapshot(&self) -> std::sync::Arc<EnumRegistry> {
        self.query_catalog_snapshot.as_ref().map_or_else(
            || self.durable.enums.snapshot(),
            |snapshot| std::sync::Arc::clone(&snapshot.enums),
        )
    }
}

impl Engine {
    /// Replace the enum values of a query result by their current labels, as `PostgreSQL` clients receive them from the type's output function. Declared column types keep the enum type.
    ///
    /// # Errors
    ///
    /// Returns an error when a result value names an enum type or label that this session's catalog no longer contains.
    pub fn render_enum_labels(&self, result: &mut uqa_sql::SQLResult) -> Result<(), SQLError> {
        uqa_sql::result::render_result_enum_labels(Some(self), result)
    }
}

impl EnumLabelCatalog for Engine {
    fn enum_type_labels(
        &self,
        type_oid: u32,
    ) -> Result<Option<std::sync::Arc<EnumTypeLabels>>, SQLError> {
        Ok(self
            .runtime
            .enum_label_cache
            .labels(&self.enum_registry_snapshot(), type_oid))
    }

    /// Labels added by the outermost transaction to a type it did not create stay unusable until commit.
    fn enum_label_uncommitted(&self, label_oid: u32) -> bool {
        self.session
            .uncommitted_enum_labels
            .lock()
            .is_uncommitted(label_oid)
    }

    /// The regtype spelling of an enum, read from the enum registry alone: label errors are raised while catalog restoration holds transaction state, where building the complete catalog projection must not be attempted.
    fn enum_type_name(&self, type_oid: u32) -> Result<Option<String>, SQLError> {
        let registry = self.enum_registry_snapshot();
        let Some(definition) = registry
            .values()
            .find(|definition| definition.oid == type_oid)
        else {
            return Ok(None);
        };
        let name = uqa_sql::expr::quote_ident(&definition.identity.name);
        let schema = &definition.identity.schema;
        let visible = schema == "pg_catalog"
            || self
                .session
                .state
                .read()
                .search_path
                .iter()
                .any(|entry| entry == schema);
        Ok(Some(if visible {
            name
        } else {
            format!("{}.{name}", uqa_sql::expr::quote_ident(schema))
        }))
    }

    fn has_enum_types(&self) -> bool {
        !self.enum_registry_snapshot().is_empty()
    }
}

impl EnumRegistryPublication for Engine {
    fn enum_registry(&self) -> EnumRegistryRead<'_> {
        Box::new(self.durable.enums.read())
    }
    fn enum_catalog(&self) -> Option<&dyn uqa_storage::CatalogFacade> {
        self.storage.catalog.as_deref()
    }
    fn enum_role_definitions(&self) -> BTreeMap<String, RoleDefinition> {
        self.durable.roles.read().clone()
    }
    fn publish_enum_definitions(&self, registry: EnumRegistry) {
        *self.durable.enums.write() = registry;
    }
}

impl EnumLabelVisibility for Engine {
    fn enum_type_created(&self, type_oid: u32) {
        if !self.session.transactions.lock().is_empty() {
            self.session
                .uncommitted_enum_labels
                .lock()
                .type_created(type_oid);
        }
    }
    fn enum_label_added(&self, type_oid: u32, label_oid: u32) {
        if !self.session.transactions.lock().is_empty() {
            self.session
                .uncommitted_enum_labels
                .lock()
                .label_added(type_oid, label_oid);
        }
    }
}
