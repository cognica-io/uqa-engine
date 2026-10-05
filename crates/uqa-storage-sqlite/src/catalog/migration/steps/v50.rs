//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Add opaque server metadata without changing the existing server value encoding.

use super::super::super::{table_exists, Catalog, Result};

pub(super) fn migrate(connection: &rusqlite::Connection) -> Result<()> {
    if !table_exists(connection, "_foreign_server_metadata")? {
        connection.execute_batch(crate::mvcc::native::foreign_servers::SQL)?;
    }
    Catalog::install_cache_revision_tracking(connection)
}
