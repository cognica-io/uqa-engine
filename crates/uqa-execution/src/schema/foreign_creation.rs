//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign server and table creation inside the caller's existing catalog transaction.
use crate::catalog::{foreign::StoredForeignTable, security::BoundTableSecurity};
use crate::schema::{
    foreign_table_alteration::ForeignTableAlterPublication,
    publication::dependencies::CatalogPublicationChanges,
    sequences::{
        implicit::{self, ImplicitSequenceContext},
        ownership::{self, ImplicitOwnershipContext},
    },
};
use crate::statement::prepared::invalidation::PreparedCatalogChange;
use std::{collections::BTreeMap, ops::DerefMut};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::DeferredCreateForeignTable,
    schema::foreign_tables::{validate_foreign_table_schema_envelope, ForeignSchemaContext},
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendResult};

pub mod entry;
mod servers;
pub use crate::catalog::foreign::reads::{
    ForeignRegistryReads, ForeignSecurityRead, ForeignServersRead, ForeignTablesRead,
};
pub type ForeignServersWrite<'a> = Box<
    dyn DerefMut<
            Target = BTreeMap<String, uqa_sql::catalog::foreign_server::ForeignServerDefinition>,
        > + 'a,
>;
pub trait ForeignCreationRegistry: ForeignRegistryReads {
    fn servers_write(&self) -> ForeignServersWrite<'_>;
}
pub trait ForeignCreationNamespace {
    fn synchronize_catalog_registries(&self) -> StorageBackendResult<()>;
    fn relation_kind_at(&self, name: &str) -> StorageBackendResult<Option<&'static str>>;
}
pub struct ForeignCreationContext<'a> {
    pub identities: crate::catalog::identity::CatalogIdentityReservationContext<'a>,
    pub creation: crate::schema::namespaces::relations::RelationCreationContext<'a>,
    pub schema: ForeignSchemaContext<'a>,
    pub namespace: &'a dyn ForeignCreationNamespace,
    pub registry: &'a dyn ForeignCreationRegistry,
    pub publication: &'a dyn ForeignTableAlterPublication,
    pub catalog: Option<&'a dyn CatalogFacade>,
    pub changes: &'a dyn CatalogPublicationChanges,
    pub sequences: ImplicitSequenceContext<'a>,
    pub ownership: ImplicitOwnershipContext<'a>,
    pub notices: &'a crate::query::NoticeQueue,
    pub allocate_identity: fn() -> StorageBackendResult<[u8; 16]>,
}
struct ForeignTableCreationTarget {
    relation: RelationIdentity,
    owner: crate::catalog::security::roles::locking::RoleBinding,
    if_not_exists: bool,
}
impl ForeignCreationContext<'_> {
    /// Resolve the new foreign table's name. `IF NOT EXISTS` skips an existing relation before the columns are described, but `heap_create_with_catalog` reports the collision only after `BuildDescForRelation` accepted them, so an early preflight passes it on.
    fn preflight_foreign_table_creation(
        &self,
        name: &str,
        if_not_exists: bool,
        report_existing: bool,
    ) -> Result<Option<(String, RelationIdentity)>, uqa_sql::SQLError> {
        self.namespace
            .synchronize_catalog_registries()
            .map_err(|error| {
                uqa_sql::SQLError::Internal(format!("refresh FDW catalog: {error}"))
            })?;
        let name = self.creation.persistent_relation_name(name)?;
        let relation = RelationIdentity::from_legacy_name(&name).map_err(|error| {
            uqa_sql::SQLError::Internal(format!("decode foreign table `{name}`: {error}"))
        })?;
        if self
            .namespace
            .relation_kind_at(&name)
            .map_err(|error| {
                uqa_sql::SQLError::Internal(format!("resolve relation `{name}`: {error}"))
            })?
            .is_some()
        {
            if if_not_exists {
                self.notices.push(
                    uqa_sql::SQLNotice::notice(format!(
                        "relation \"{}\" already exists, skipping",
                        relation.name
                    ))
                    .with_sqlstate("42P07"),
                );
                return Ok(None);
            }
            if !report_existing {
                return Ok(Some((name, relation)));
            }
            return Err(uqa_sql::SQLError::Routine {
                sqlstate: "42P07".into(),
                message: format!("relation \"{}\" already exists", relation.name),
            });
        }
        {
            let tables = self.registry.tables();
            let table_security = self.registry.security();
            if tables.contains_key(&relation) {
                if !table_security.contains_key(&relation) {
                    return Err(uqa_sql::SQLError::Internal(format!(
                        "foreign table `{name}` has no loaded security metadata"
                    )));
                }
                return Err(uqa_sql::SQLError::Internal(format!(
                    "foreign table `{name}` appeared after relation collision preflight"
                )));
            }
            if table_security.contains_key(&relation) {
                return Err(uqa_sql::SQLError::Internal(format!(
                    "foreign table security metadata exists without table `{name}`"
                )));
            }
        }
        Ok(Some((name, relation)))
    }
    pub fn register_foreign_table_inner(
        &self,
        name: &str,
        server_name: String,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        checks: Vec<uqa_sql::ast::TableCheck>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), uqa_sql::SQLError> {
        self.register_foreign_table_statement(uqa_sql::ast::CreateForeignTable {
            name: name.to_string(),
            server_name,
            columns,
            checks,
            not_null_declarations: None,
            options,
            if_not_exists,
        })
    }
    pub fn register_foreign_table_statement(
        &self,
        statement: uqa_sql::ast::CreateForeignTable,
    ) -> Result<(), SQLError> {
        if !statement.if_not_exists {
            validate_foreign_table_schema_envelope(&statement.columns)?;
        }
        let owner = self.creation.bind_owner()?;
        let Some((_, relation)) =
            self.preflight_foreign_table_creation(&statement.name, statement.if_not_exists, false)?
        else {
            return Ok(());
        };
        if statement.if_not_exists {
            validate_foreign_table_schema_envelope(&statement.columns)?;
        }
        self.register_foreign_table_after_preflight(
            ForeignTableCreationTarget {
                relation,
                owner,
                if_not_exists: statement.if_not_exists,
            },
            statement,
        )
    }
    fn register_foreign_table_after_preflight(
        &self,
        target: ForeignTableCreationTarget,
        mut statement: uqa_sql::ast::CreateForeignTable,
    ) -> Result<(), uqa_sql::SQLError> {
        let name = target.relation.qualified_name();
        let name = name.as_str();
        for column in &mut statement.columns {
            column.ty = uqa_sql::type_resolution::resolve_declared_column_type(
                self.schema.types,
                &column.ty,
            )?;
        }
        for column in &statement.columns {
            self.schema.types.require_type_usage(&column.ty)?;
        }
        self.creation.retain_owner(&target.owner)?;
        let Some((_, relation)) =
            self.preflight_foreign_table_creation(name, target.if_not_exists, true)?
        else {
            return Ok(());
        };
        implicit::materialize_implicit_sequences(
            &self.sequences,
            "CREATE FOREIGN TABLE",
            name,
            &mut statement.columns,
            uqa_sql::ast::RelationPersistence::Permanent,
        )?;
        let catalog_oids = self.prepare_foreign_table_definition(&relation, &mut statement)?;
        let server_reference = self
            .registry
            .servers()
            .get(&statement.server_name)
            .map(crate::catalog::foreign::ForeignServerReference::from)
            .ok_or_else(|| {
                uqa_sql::schema::foreign_servers::missing_server(&statement.server_name)
            })?;
        self.creation.reserve_row_type_name(name)?;
        let object_id = (self.allocate_identity)().map_err(|error| {
            uqa_sql::SQLError::Internal(format!(
                "allocate foreign table `{name}` object identity: {error}"
            ))
        })?;
        let owner_columns = statement.columns.clone();
        let table = StoredForeignTable {
            name: name.to_string(),
            object_id,
            catalog_oids: Some(catalog_oids),
            row_type_array_name: Some(crate::schema::types::arrays::reserve_array_name(
                &self.creation,
                &relation.schema,
                &relation.name,
            )?),
            server_name: statement.server_name,
            server_reference: Some(server_reference),
            columns: statement.columns,
            checks: statement.checks,
            options: statement.options.into_iter().collect(),
        };
        let security = BoundTableSecurity::owner(target.owner.identity());
        let mut tables = self.publication.tables_write();
        let mut table_security = self.publication.security_write();
        if tables.contains_key(&relation) || table_security.contains_key(&relation) {
            return Err(uqa_sql::SQLError::Internal(format!(
                "foreign table `{name}` changed during creation"
            )));
        }
        if let Some(catalog) = self.catalog {
            catalog
                .save_foreign_table(&table.catalog_row(&relation, &security).map_err(|error| {
                    uqa_sql::SQLError::Internal(format!(
                        "serialize foreign table `{name}`: {error}"
                    ))
                })?)
                .map_err(|error| {
                    uqa_sql::SQLError::Internal(format!("persist foreign table `{name}`: {error}"))
                })?;
        }
        tables.insert(relation.clone(), table);
        table_security.insert(relation, security);
        drop(table_security);
        drop(tables);
        ownership::attach_column_owners(&self.ownership, name, object_id, &owner_columns).map_err(
            |error| {
                uqa_sql::SQLError::Internal(format!(
                    "attach foreign table `{name}` sequence ownership: {error}"
                ))
            },
        )?;
        self.changes.catalog_registry_changed();
        self.changes
            .prepared_catalog_changed(PreparedCatalogChange::Relation(catalog_oids.relation));
        Ok(())
    }
    fn prepare_foreign_table_definition(
        &self,
        relation: &RelationIdentity,
        statement: &mut uqa_sql::ast::CreateForeignTable,
    ) -> Result<uqa_sql::catalog::relation_oids::RelationCatalogOids, SQLError> {
        // `DefineRelation` allocates the relation's OIDs before those of its constraints.
        let catalog_oids = self
            .identities
            .allocator(crate::catalog::identity::allocate_catalog_object_id)
            .allocate_relation_oids(
                uqa_sql::catalog::relation_oids::RelationOidKind::ForeignTable,
                relation,
            )?;
        let not_nulls = statement.not_null_declarations.take().map(|declarations| {
            uqa_sql::schema::foreign_tables::ForeignTableNotNulls {
                relation_oid: catalog_oids.relation,
                declarations,
            }
        });
        self.schema.prepare_foreign_table_schema(
            &relation.qualified_name(),
            &mut statement.columns,
            &mut statement.checks,
            &mut self
                .identities
                .allocator(crate::catalog::identity::allocate_catalog_object_id),
            &crate::schema::constraints::names::name_scope(
                &self.identities.catalog.current_catalog_snapshot(),
                relation,
            ),
            not_nulls,
        )?;
        Ok(catalog_oids)
    }
    pub fn register_deferred_foreign_table(
        &self,
        deferred: DeferredCreateForeignTable,
    ) -> Result<(), SQLError> {
        let owner = self.creation.bind_owner()?;
        let Some((_, relation)) =
            self.preflight_foreign_table_creation(&deferred.name, deferred.if_not_exists, false)?
        else {
            return Ok(());
        };
        let statement =
            uqa_sql::resolve_deferred_create_foreign_table(&deferred, self.schema.types)?;
        validate_foreign_table_schema_envelope(&statement.columns)?;
        self.register_foreign_table_after_preflight(
            ForeignTableCreationTarget {
                relation,
                owner,
                if_not_exists: deferred.if_not_exists,
            },
            statement,
        )
    }
}
