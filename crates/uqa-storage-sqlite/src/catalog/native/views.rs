//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View definitions, ownership and ACLs on the native catalog snapshot.

use super::{
    optional_text, string, text, Catalog, Family, NativeRecordOwner, RelationRecord, Result,
};
use crate::catalog::{RelationIdentity, ViewRow};
use rusqlite::types::ValueRef;

impl Catalog {
    pub(in crate::catalog) fn save_native_view(&self, view: &ViewRow) -> Result<Option<()>> {
        if !self.conn.is_native_record_session() {
            return Ok(None);
        }
        let acl = view.acl.as_ref().map(serde_json::to_string).transpose()?;
        let columns = serde_json::to_string(&view.column_acls)?;
        self.save_native_relation(
            RelationRecord::View,
            &view.relation,
            &[
                text(&view.relation.schema),
                text(&view.relation.name),
                text("view"),
                text(&view.definition_json),
                text(&view.role_owner),
                optional_text(acl.as_deref()),
                text(&columns),
            ],
        )
    }

    pub(in crate::catalog) fn load_native_views(&self) -> Result<Option<Vec<ViewRow>>> {
        self.read_native(|snapshot| {
            let mut views = Vec::new();
            snapshot.visit_rows(
                Family::Views,
                Some(NativeRecordOwner::Database(snapshot.database)),
                &[],
                |row| {
                    views.push(ViewRow {
                        relation: RelationIdentity::new(string(row[0])?, string(row[1])?),
                        definition_json: string(row[3])?,
                        role_owner: string(row[4])?,
                        acl: if row[5] == ValueRef::Null {
                            None
                        } else {
                            Some(serde_json::from_str(&string(row[5])?)?)
                        },
                        column_acls: if row[6] == ValueRef::Null {
                            std::collections::BTreeMap::new()
                        } else {
                            serde_json::from_str(&string(row[6])?)?
                        },
                    });
                    Ok(())
                },
            )?;
            Ok(views)
        })
    }
}
