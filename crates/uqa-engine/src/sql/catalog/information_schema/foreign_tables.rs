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
    ColumnPrivilegeCatalogRow, SQLColumnDef,
};
use crate::engine_capabilities::CatalogReadView;

pub(super) fn foreign_information_schema_column(column: &uqa_fdw::ColumnDef) -> SQLColumnDef {
    SQLColumnDef {
        name: column.name.clone(),
        ty: crate::engine_fdw::fdw_column_type_to_sql(&column.ty),
        object_id: None,
        missing_value: None,
        primary_key: false,
        not_null: false,
        not_null_explicit: false,
        not_null_name: None,
        not_null_validated: true,
        not_null_no_inherit: false,
        auto_increment: None,
        unique: false,
        default: None,
        generated: None,
        check: None,
        check_name: None,
        check_enforced: true,
        check_validated: true,
        check_no_inherit: false,
        references: None,
    }
}

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
