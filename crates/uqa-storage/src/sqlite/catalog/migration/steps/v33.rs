//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persist schema ownership and ACL metadata.

use super::super::super::{Catalog, Result};

pub(super) fn migrate(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    let columns = Catalog::table_columns(tx, "_schemas")?.unwrap_or_default();
    if !columns.contains_key("role_owner") {
        tx.execute_batch(
            "ALTER TABLE _schemas ADD COLUMN role_owner TEXT NOT NULL DEFAULT 'uqa';",
        )?;
    }
    if !columns.contains_key("acl_json") {
        tx.execute_batch("ALTER TABLE _schemas ADD COLUMN acl_json TEXT;")?;
    }
    let public_acl = serde_json::to_string(&public_schema_acl())?;
    tx.execute(
        "UPDATE _schemas SET acl_json = ?1 WHERE name = 'public' AND acl_json IS NULL",
        [public_acl],
    )?;
    Ok(())
}

fn public_schema_acl() -> Vec<crate::catalog::SchemaAclEntry> {
    crate::catalog::SchemaRow::legacy("public")
        .acl
        .expect("public schema has an explicit default ACL")
}
