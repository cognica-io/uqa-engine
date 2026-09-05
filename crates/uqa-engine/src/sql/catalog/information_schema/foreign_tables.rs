//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-table `information_schema` column metadata.

use std::collections::BTreeSet;

use uqa_sql::SQLError;

use super::{
    default_table_acl_entry, insert_column_privilege_rows, split_schema_name,
    ColumnPrivilegeCatalogRow,
};
use crate::engine_capabilities::CatalogReadView;

pub(super) fn insert_foreign_table_column_privileges(
    catalog: &CatalogReadView,
    privileges: &mut BTreeSet<ColumnPrivilegeCatalogRow>,
) -> Result<(), SQLError> {
    for (foreign_name, foreign_table) in catalog.foreign_tables() {
        let (schema, table) = split_schema_name(&foreign_name)?;
        let security = catalog.foreign_table_security(&foreign_name)?;
        let default_acl;
        let table_acl = if let Some(acl) = security.acl.as_deref() {
            acl
        } else {
            default_acl = [default_table_acl_entry(&security.role_owner)];
            &default_acl
        };
        for column in &foreign_table.columns {
            for entry in table_acl {
                insert_column_privilege_rows(
                    privileges,
                    &schema,
                    &table,
                    &column.name,
                    &security.role_owner,
                    entry,
                );
            }
            if let Some(column_acl) = security.column_acls.get(&column.name) {
                for entry in column_acl {
                    insert_column_privilege_rows(
                        privileges,
                        &schema,
                        &table,
                        &column.name,
                        &security.role_owner,
                        entry,
                    );
                }
            }
        }
    }
    Ok(())
}
