//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema privilege queries retain their original participant while SQL owns target and privilege rules.

use crate::catalog::{context::CatalogContext, CatalogReadView};
use uqa_core::Value;
use uqa_sql::{
    catalog::security::schema_inquiry::{
        GraphNamespaceRead, SchemaPrivilegeCatalog, SchemaPrivilegeInquiry, SchemaRegistryRead,
    },
    SQLError,
};

struct ObservedNamespaces<'a> {
    source: &'a dyn SchemaPrivilegeCatalog,
    reads: &'a CatalogReadView,
}

impl SchemaPrivilegeCatalog for ObservedNamespaces<'_> {
    fn refresh_namespace_catalog(&self) -> Result<(), SQLError> {
        self.source.refresh_namespace_catalog()
    }

    fn schemas(&self) -> SchemaRegistryRead<'_> {
        self.source.schemas()
    }

    fn graphs(&self) -> Box<dyn GraphNamespaceRead + '_> {
        self.source.graphs()
    }

    fn temporary_namespace_allocated(&self) -> bool {
        self.source.temporary_namespace_allocated()
    }

    fn temporary_schema_name(&self) -> String {
        self.source.temporary_schema_name()
    }

    fn observe_namespace_lookup(&self, name: Option<&str>) -> Result<(), SQLError> {
        self.source.observe_namespace_lookup(name)?;
        let Some(name) = name else {
            return self.reads.observe_graph_names();
        };
        let durable = self.source.schemas().contains_key(name);
        if durable
            || uqa_sql::catalog::is_virtual_system_schema(name)
            || (self.source.temporary_namespace_allocated()
                && name == self.source.temporary_schema_name())
        {
            return Ok(());
        }
        self.reads.observe_graph_name(name)
    }
}

pub fn has_schema_privilege_value(
    context: &CatalogContext<'_>,
    inquiry: &SchemaPrivilegeInquiry<'_>,
    arguments: &[Value],
) -> Result<Value, SQLError> {
    context.with_query_reads(|context| {
        let reads = context.catalog_read_view();
        let catalog = ObservedNamespaces {
            source: inquiry.catalog,
            reads: &reads,
        };
        SchemaPrivilegeInquiry {
            catalog: &catalog,
            names: inquiry.names,
            roles: inquiry.roles,
        }
        .has_schema_privilege_value(arguments)
    })
}
