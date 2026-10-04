//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{validate_index_declaration, IndexBuildCatalog};
use uqa_sql::{
    ast::{Expr, PartitionSpec, PartitionStrategy, Statement, TableHierarchy},
    SQLError,
};

struct DeclarationCatalog;

impl IndexBuildCatalog for DeclarationCatalog {
    fn table_hierarchy(&self, table: &str) -> Result<TableHierarchy, SQLError> {
        assert_eq!(table, "items");
        Ok(TableHierarchy {
            partition_spec: Some(PartitionSpec {
                strategy: PartitionStrategy::Range,
                keys: vec![Expr::Column("tenant".into())],
            }),
            ..TableHierarchy::default()
        })
    }
    fn scan_tables(&self, _: &str) -> Result<Vec<String>, SQLError> {
        panic!("declaration validation must not enumerate build inputs")
    }
    fn partition_tree(
        &self,
        _: &str,
    ) -> Result<Vec<uqa_sql::semantics::partition::PartitionTreeNode>, SQLError> {
        panic!("declaration validation must not enumerate build inputs")
    }
}

#[test]
fn index_declaration_checks_partition_coverage_without_scanning_rows() {
    for (sql, expected) in [
        (
            "CREATE UNIQUE INDEX i ON items(id)",
            Some("unique constraint on partitioned table must include all partitioning columns"),
        ),
        ("CREATE UNIQUE INDEX i ON items(tenant,id)", None),
        (
            "CREATE UNIQUE INDEX i ON items USING diskann(embedding)",
            Some("access method \"diskann\" does not support unique indexes"),
        ),
        ("CREATE INDEX i ON items(id)", None),
    ] {
        let Statement::CreateIndex(statement) = uqa_sql::compiler::compile(sql).unwrap().remove(0)
        else {
            panic!("index statement")
        };
        let result = validate_index_declaration(&DeclarationCatalog, &statement);
        assert_eq!(
            result.as_ref().err().and_then(SQLError::sqlstate),
            expected.map(|_| "0A000")
        );
        assert_eq!(
            result.err().map(|error| error.to_string()).as_deref(),
            expected
        );
    }
}
