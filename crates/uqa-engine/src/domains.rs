//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable domain identities and catalog publication.

use uqa_sql::ast::ColumnType;
use uqa_storage::CatalogFacade;

use crate::{Engine, StorageBackendResult};

pub(crate) use uqa_sql::catalog::domain::StoredDomain;

impl Engine {
    pub(crate) fn domain_default_expression(&self, ty: &ColumnType) -> Option<uqa_sql::ast::Expr> {
        uqa_sql::catalog::domain::domain_default_expression(self, ty)
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
    pub(crate) fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        self.durable
            .domains
            .read()
            .values()
            .find(|domain| domain.oid == oid)
            .cloned()
    }

    pub(crate) fn restore_domains_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        allow_migration: bool,
    ) -> StorageBackendResult<()> {
        let registry = uqa_execution::catalog::domain::restore(
            catalog,
            &self.durable.roles.read(),
            allow_migration,
        )?;
        *self.durable.domains.write() = registry;
        Ok(())
    }
}

impl uqa_sql::catalog::domain::DomainCatalog for Engine {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        Engine::domain_by_oid(self, oid)
    }
}
