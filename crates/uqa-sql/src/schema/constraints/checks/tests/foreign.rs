//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign CHECKs bind against the row type published by their declaration or restoration.

use super::*;
use crate::schema::dependencies::regclass::SchemaReferenceCatalog;
use crate::schema::foreign_tables::ForeignSchemaContext;
use crate::schema::sequences::implicit_ownership::StoredSequenceNames;
use crate::semantics::conflict::InferenceBindingScope;

impl InferenceBindingScope for Catalog {
    fn binding_scope(&self) -> Result<crate::binding::snapshot::BindingSnapshot, SQLError> {
        Ok(crate::binding::fixture::empty_binding_context().into())
    }
}

impl SchemaReferenceCatalog for Catalog {
    fn loaded_relation_name(&self, _: &str) -> Result<Option<String>, String> {
        panic!("these CHECKs contain no literal relation names");
    }

    fn bound_relation_oid(&self, _: &str) -> Result<Option<i64>, String> {
        panic!("these CHECKs contain no literal relation names");
    }

    fn visible_relation_oid(&self, _: &str) -> Result<Option<i64>, String> {
        panic!("these CHECKs contain no literal relation names");
    }

    fn sequence_for_binding(&self, _: &str) -> Result<String, String> {
        panic!("these CHECKs contain no sequence call");
    }
}

impl StoredSequenceNames for Catalog {
    fn stored_sequence_name(&self, _: &str) -> Result<String, String> {
        panic!("these CHECKs contain no sequence call");
    }
}

#[test]
fn foreign_checks_bind_declared_and_restored_row_types() {
    // PostgreSQL 18.4 accepts the whole-row and tableoid CHECK declarations captured in the independent relation and row-scope references.
    let context = ForeignSchemaContext {
        types: &Catalog,
        schema: &Catalog,
        bindings: &Catalog,
        references: &Catalog,
        sequences: &Catalog,
    };
    for source in ["foreign_named IS NOT NULL", "tableoid > 0"] {
        let sql = format!(
            "CREATE FOREIGN TABLE foreign_named(n integer,CHECK({source})) SERVER server_no_handler"
        );
        let Statement::CreateForeignTableDefinition(deferred) =
            crate::compile(&sql).unwrap().remove(0)
        else {
            panic!("CREATE FOREIGN TABLE");
        };
        let mut table = crate::resolve_deferred_create_foreign_table(&deferred, &Catalog).unwrap();
        let mut counter = 0_u8;
        let mut allocate = |_: &str| {
            counter += 1;
            Ok([counter; 16])
        };
        context
            .prepare_foreign_table_schema(
                &table.name,
                &mut table.columns,
                &mut table.checks,
                &mut allocate,
                &crate::schema::constraint_metadata::ConstraintNameScope::default(),
                None,
            )
            .unwrap();
        let original = serde_json::to_value((&table.columns, &table.checks)).unwrap();
        context
            .prepare_stored_foreign_table_schema(
                &table.name,
                &mut table.columns,
                &mut table.checks,
                &[],
                &mut |_: &str| panic!("restoring an existing CHECK must preserve its identity"),
            )
            .unwrap();
        assert_eq!(
            serde_json::to_value((&table.columns, &table.checks)).unwrap(),
            original,
            "{source}"
        );
    }
}
