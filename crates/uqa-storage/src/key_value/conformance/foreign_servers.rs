//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-server definitions and SQL metadata share a single retained record.

use crate::{
    CatalogFacade, ForeignServerRow, KeyValueCatalog, KeyValueStore, StorageBackendResult,
};
use std::sync::Arc;

fn final_row() -> ForeignServerRow {
    ForeignServerRow {
        name: "foreign-metadata-contract".into(),
        fdw_type: "memory".into(),
        options_json: r#"{"option":"final"}"#.into(),
        metadata_json: Some(r#"{"version":1,"identity":"final"}"#.into()),
    }
}

/// Verify atomic metadata replacement, retained reads, legacy updates, deletion and undo on disposable provider sessions.
pub fn verify_foreign_server_rows(
    reader: &Arc<dyn KeyValueStore>,
    writer: &Arc<dyn KeyValueStore>,
) -> StorageBackendResult<()> {
    let a = KeyValueCatalog::new(reader.clone());
    let b = KeyValueCatalog::new(writer.clone());
    let mut row = final_row();
    row.metadata_json = None;
    b.save_foreign_server(&row.name, &row.fdw_type, &row.options_json)?;
    assert_eq!(a.load_foreign_server_rows()?, [row.clone()]);
    row.metadata_json = Some("first identity".into());
    b.save_foreign_server_row(&row)?;
    reader.begin_read_transaction()?;
    assert_eq!(a.load_foreign_server_rows()?, [row.clone()]);
    b.save_foreign_server_row(&final_row())?;
    assert_eq!(a.load_foreign_server_rows()?, [row]);
    reader.commit_transaction()?;
    assert_eq!(a.load_foreign_server_rows()?, [final_row()]);

    writer.begin_transaction()?;
    writer.savepoint("before_server_update")?;
    b.save_foreign_server(&final_row().name, "changed-wrapper", "changed options")?;
    let updated = b.load_foreign_server_rows()?.remove(0);
    assert_eq!(updated.fdw_type, "changed-wrapper");
    assert_eq!(updated.options_json, "changed options");
    assert_eq!(updated.metadata_json, final_row().metadata_json);
    b.drop_foreign_server(&updated.name)?;
    assert!(b.load_foreign_server_rows()?.is_empty());
    assert_eq!(a.load_foreign_server_rows()?, [final_row()]);
    writer.rollback_to_savepoint("before_server_update")?;
    assert_eq!(b.load_foreign_server_rows()?, [final_row()]);
    writer.commit_transaction()?;
    b.drop_foreign_server(&final_row().name)?;
    b.save_foreign_server(&final_row().name, "new-wrapper", "new options")?;
    assert_eq!(b.load_foreign_server_rows()?[0].metadata_json, None);
    b.save_foreign_server_row(&final_row())?;
    Ok(())
}

/// The row left by the session contract must reopen without losing either value field.
pub fn verify_foreign_server_reopen(store: &Arc<dyn KeyValueStore>) -> StorageBackendResult<()> {
    let catalog = KeyValueCatalog::new(store.clone());
    assert_eq!(catalog.load_foreign_server_rows()?, [final_row()]);
    assert_eq!(
        catalog.load_foreign_servers()?,
        [(
            final_row().name,
            final_row().fdw_type,
            final_row().options_json
        )]
    );
    Ok(())
}
