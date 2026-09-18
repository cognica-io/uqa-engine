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

impl Catalog {
    pub(in crate::catalog) fn save_native_view(&self, view: &ViewRow) -> Result<Option<()>> {
        if !self.conn.is_native_record_session() {
            return Ok(None);
        }
        let (security_owner, acl, columns) =
            super::super::role_security::encode_relation(&view.security)?;
        self.save_native_relation(
            RelationRecord::View,
            &view.relation,
            &[
                text(&view.relation.schema),
                text(&view.relation.name),
                text("view"),
                text(&view.definition_json),
                (&security_owner).into(),
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
                        security: super::super::role_security::decode_relation_cells(
                            row[4], row[5], row[6],
                        )?,
                    });
                    Ok(())
                },
            )?;
            Ok(views)
        })
    }
}
