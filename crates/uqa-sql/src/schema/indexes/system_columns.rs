//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! CREATE INDEX over a system column, which `PostgreSQL` resolves like any attribute and then refuses to index.

use crate::ast::{ColumnDef, CreateIndex, Expr, IndexKey, PartitionSpec};
use crate::schema::columns::POSTGRES_SYSTEM_COLUMNS;
use crate::schema::keys::definition::{
    is_system_column, resolve_system_key_attribute, system_column_index,
};
use crate::SQLError;

fn reads_system_column(expression: &Expr) -> bool {
    POSTGRES_SYSTEM_COLUMNS.iter().any(|column| {
        crate::schema::dependencies::schema_expr_references_column(expression, column)
    })
}

/// Fail a CREATE INDEX that reads a system column as `DefineIndex` fails it: once the access method and each attribute are resolved and a unique index is matched against the partition key. `columns` are the table's columns and `partition` its partition key; a statement that reads no system column passes.
pub fn reject_system_column_index(
    columns: &[ColumnDef],
    statement: &CreateIndex,
    partition: Option<&PartitionSpec>,
) -> Result<(), SQLError> {
    let system = statement.columns.iter().any(|key| match key {
        IndexKey::Column(name) => is_system_column(name),
        IndexKey::Expression(expression) => reads_system_column(expression),
    }) || statement
        .included_columns
        .iter()
        .any(|name| is_system_column(name))
        || statement
            .predicate
            .as_deref()
            .is_some_and(reads_system_column);
    if !system {
        return Ok(());
    }
    let access_method = super::options::index_access_method(statement)?;
    if statement.unique {
        super::unique::validate_unique_index_method(statement)?;
    }
    let declared = |name: &str| columns.iter().any(|column| column.name == name);
    for key in &statement.columns {
        let IndexKey::Column(name) = key else {
            continue;
        };
        if is_system_column(name) {
            resolve_system_key_attribute(name, &access_method)?;
        } else if !declared(name) {
            return Err(SQLError::UnknownColumn(name.clone()));
        }
    }
    if let Some(name) = statement
        .included_columns
        .iter()
        .find(|name| !declared(name) && !is_system_column(name))
    {
        return Err(SQLError::UnknownColumn(name.clone()));
    }
    if let Some(partition) = partition.filter(|_| statement.unique) {
        super::unique::validate_unique_index_partition_key(statement, partition)?;
    }
    Err(system_column_index())
}

#[cfg(test)]
mod tests {
    use super::reject_system_column_index;
    use crate::ast::{CreateIndex, Expr, PartitionSpec, PartitionStrategy, Statement};

    const SYSTEM: &str = "index creation on system columns is not supported";

    fn index(sql: &str) -> CreateIndex {
        let Statement::CreateIndex(statement) = crate::compiler::compile(sql).unwrap().remove(0)
        else {
            panic!("index statement");
        };
        statement
    }

    fn columns() -> Vec<crate::ast::ColumnDef> {
        let Statement::CreateTable(table) =
            crate::compiler::compile("CREATE TABLE t (a int, b int)")
                .unwrap()
                .remove(0)
        else {
            panic!("table statement");
        };
        table.columns
    }

    fn partition() -> PartitionSpec {
        PartitionSpec {
            strategy: PartitionStrategy::Range,
            keys: vec![Expr::Column("a".into())],
        }
    }

    fn rejected(sql: &str, partition: Option<&PartitionSpec>) -> (String, String) {
        let error = reject_system_column_index(&columns(), &index(sql), partition).unwrap_err();
        (
            error.sqlstate().unwrap_or_default().to_string(),
            error.to_string(),
        )
    }

    fn unordered(ty: &str, method: &str) -> (String, String) {
        (
            "42704".into(),
            format!("data type {ty} has no default operator class for access method \"{method}\""),
        )
    }

    #[test]
    fn an_index_over_a_system_column_fails_after_its_attributes_are_resolved() {
        let system = ("0A000".to_string(), SYSTEM.to_string());
        let missing = (
            "42703".to_string(),
            "column \"zz\" does not exist".to_string(),
        );
        for (sql, expected) in [
            ("CREATE INDEX ON t (ctid)", system.clone()),
            ("CREATE INDEX ON t (tableoid)", system.clone()),
            ("CREATE INDEX ON t (xmin)", unordered("xid", "btree")),
            ("CREATE INDEX ON t (cmax)", unordered("cid", "btree")),
            (
                "CREATE INDEX ON t USING gin (ctid)",
                unordered("tid", "gin"),
            ),
            ("CREATE INDEX ON t (a) INCLUDE (xmin)", system.clone()),
            (
                "CREATE INDEX ON t (a) WHERE ctid IS NOT NULL",
                system.clone(),
            ),
            ("CREATE INDEX ON t (a, (ctid::text))", system.clone()),
            ("CREATE INDEX ON t (zz, ctid)", missing.clone()),
            ("CREATE INDEX ON t (xmin, zz)", unordered("xid", "btree")),
            ("CREATE UNIQUE INDEX ON t (b) INCLUDE (ctid, zz)", missing),
        ] {
            assert_eq!(rejected(sql, None), expected, "{sql}");
        }
        for sql in [
            "CREATE INDEX ON t (a)",
            "CREATE UNIQUE INDEX ON t (b, (a + 1)) INCLUDE (a) WHERE a > 0",
        ] {
            reject_system_column_index(&columns(), &index(sql), Some(&partition())).unwrap();
        }
    }

    #[test]
    fn a_unique_index_is_matched_against_the_partition_key_before_its_system_columns() {
        let partition = partition();
        assert_eq!(
            rejected("CREATE UNIQUE INDEX ON t (ctid)", Some(&partition)),
            (
                "0A000".into(),
                "unique constraint on partitioned table must include all partitioning columns"
                    .into()
            )
        );
        for sql in [
            "CREATE UNIQUE INDEX ON t (a, ctid)",
            "CREATE INDEX ON t (ctid)",
        ] {
            assert_eq!(
                rejected(sql, Some(&partition)),
                ("0A000".into(), SYSTEM.into()),
                "{sql}"
            );
        }
    }
}
