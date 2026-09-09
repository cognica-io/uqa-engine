//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable domain identities and catalog publication.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uqa_sql::ast::{ColumnType, CreateDomain};
use uqa_sql::SQLError;
use uqa_storage::CatalogFacade;

use crate::{Engine, RelationIdentity, StorageBackendResult};

const DOMAINS_METADATA_KEY: &str = "sql_domains_json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct StoredDomain {
    pub(crate) object_id: [u8; 16],
    pub(crate) oid: u32,
    pub(crate) identity: RelationIdentity,
    pub(crate) owner: String,
    pub(crate) definition: CreateDomain,
}

impl StoredDomain {
    pub(crate) fn column_type(&self) -> ColumnType {
        ColumnType::Domain {
            schema: self.identity.schema.clone(),
            name: self.identity.name.clone(),
            oid: self.oid,
            base: Box::new(self.definition.base.clone()),
        }
    }
}

impl Engine {
    pub(crate) fn domain_default_expression(&self, ty: &ColumnType) -> Option<uqa_sql::ast::Expr> {
        let ColumnType::Domain { oid, base, .. } = ty else {
            return None;
        };
        self.domain_by_oid(*oid)
            .and_then(|domain| domain.definition.default)
            .or_else(|| self.domain_default_expression(base))
    }

    pub(crate) fn try_column_insert_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::Expr>> {
        if let Some(default) = self.try_column_default_expr(table, column)? {
            return Ok(Some(default));
        }
        let columns = self.try_describe_table(table)?.unwrap_or_default();
        let Some(column) = columns.iter().find(|definition| definition.name == column) else {
            return Ok(None);
        };
        Ok(self.domain_default_expression(&column.ty).or_else(|| {
            matches!(column.ty, ColumnType::Domain { .. })
                .then_some(uqa_sql::ast::Expr::Literal(uqa_core::Value::Null))
        }))
    }
    pub(crate) fn resolve_domain_type(&self, name: &str) -> Option<ColumnType> {
        let names = uqa_sql::compiler::parse_regobject_name(name)?;
        let domains = self.durable.domains.read();
        if let [schema, local] = names.as_slice() {
            return domains
                .values()
                .find(|domain| domain.identity.schema == *schema && domain.identity.name == *local)
                .map(StoredDomain::column_type);
        }
        let [local] = names.as_slice() else {
            return None;
        };
        let search_path = self.session.state.read().search_path.clone();
        for schema in search_path {
            if let Some(domain) = domains
                .values()
                .find(|domain| domain.identity.schema == schema && domain.identity.name == *local)
            {
                return Some(domain.column_type());
            }
        }
        None
    }

    pub(crate) fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        self.durable
            .domains
            .read()
            .values()
            .find(|domain| domain.oid == oid)
            .cloned()
    }

    pub(crate) fn publish_domain(&self, domain: StoredDomain) -> Result<(), SQLError> {
        let mut registry = self.durable.domains.write();
        let mut next = registry.clone();
        next.insert(domain.identity.qualified_name(), domain);
        self.persist_domains(&next)?;
        *registry = next;
        drop(registry);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub(crate) fn persist_domains(
        &self,
        registry: &BTreeMap<String, StoredDomain>,
    ) -> Result<(), SQLError> {
        if let Some(catalog) = &self.storage.catalog {
            let json = serde_json::to_string(registry).map_err(|error| {
                SQLError::Internal(format!("serialize domain catalog: {error}"))
            })?;
            catalog
                .set_metadata(DOMAINS_METADATA_KEY, &json)
                .map_err(|error| SQLError::Internal(format!("persist domain catalog: {error}")))?;
        }
        Ok(())
    }

    pub(crate) fn restore_domains_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        let registry = catalog
            .get_metadata(DOMAINS_METADATA_KEY)?
            .map(|json| serde_json::from_str(&json))
            .transpose()?
            .unwrap_or_default();
        *self.durable.domains.write() = registry;
        Ok(())
    }
}
