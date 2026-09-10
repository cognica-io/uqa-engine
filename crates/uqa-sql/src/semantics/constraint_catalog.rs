//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only relation constraint declarations.
use crate::{
    ast::{ColumnType, ForeignKey, TableCheck},
    SQLError,
};
pub trait ConstraintCatalog: super::conflict::ConflictCatalog {
    fn try_check_constraint_definitions(&self, table: &str) -> Result<Vec<TableCheck>, String>;
    fn try_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, String>;
    fn column_type(&self, table: &str, column: &str) -> Result<Option<ColumnType>, String>;
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        descendants: bool,
    ) -> Result<Vec<String>, SQLError>;
}
