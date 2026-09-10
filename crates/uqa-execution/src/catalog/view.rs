//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub use uqa_sql::catalog::{stored_view::StoredView, view::StoredViewKind};

use uqa_core::RelationIdentity;
use uqa_storage::ViewRow;

pub fn catalog_view_row(
    relation: &RelationIdentity,
    view: &StoredView,
) -> Result<ViewRow, serde_json::Error> {
    Ok(ViewRow {
        relation: relation.clone(),
        role_owner: view.role_owner.clone(),
        acl: view.acl.clone(),
        column_acls: view.column_acls.clone(),
        definition_json: serde_json::to_string(view)?,
    })
}
