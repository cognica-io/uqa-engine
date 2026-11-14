//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{expect, expect_eq};
use crate::catalog::relation_acl::RelationAclTuple;
use crate::{
    catalog::{
        foreign_load::{CatalogForeignTableLoad, CatalogForeignTables},
        input::CatalogInputLimits,
    },
    read_control::StorageReadControl,
    ForeignTableRow, PersistentStorageSession, RelationIdentity, RelationSecurityRow,
    StorageBackendResult,
};
use uqa_core::{
    catalog_acl::TablePrivileges,
    catalog_role::{BoundAclEntry, RoleIdentity},
};

fn finish(input: &mut CatalogForeignTableLoad) -> StorageBackendResult<CatalogForeignTables> {
    for _ in 0..65536 {
        if let Some(rows) = input.advance(1)? {
            return Ok(rows);
        }
    }
    Err(crate::StorageBackendError::Other(
        "foreign table input stopped progressing".into(),
    ))
}

/// Verify a disposable provider's complete ordered foreign table input over its selected private view, independently of the originating session's later lifetime.
pub fn verify_catalog_foreign_table_input(
    source: PersistentStorageSession,
) -> StorageBackendResult<()> {
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 65536,
    };
    source.catalog.initialize_storage()?;
    source.catalog.save_schema("foreign_input")?;
    for name in ["z", "public", "a_longer", "한글 table", "removed"] {
        source.catalog.save_foreign_table(&row(name))?;
    }
    let entries = vec![
        BoundAclEntry {
            role: None,
            grantor: RoleIdentity::BOOTSTRAP,
            privileges: TablePrivileges {
                select: true,
                ..Default::default()
            },
            grant_options: TablePrivileges::default(),
        };
        64
    ];
    let relation = row("a_longer").relation;
    source.catalog.save_relation_acl(
        &relation,
        None,
        &RelationAclTuple::new(Some(entries.clone()))?,
    )?;
    source.catalog.save_relation_acl(
        &relation,
        Some("payload"),
        &RelationAclTuple::new(Some(entries))?,
    )?;
    source.backend.begin_transaction()?;
    let mut replacement = row("public");
    replacement.server_name = "private replacement".into();
    source.catalog.save_foreign_table(&replacement)?;
    source.catalog.save_foreign_table(&row("private"))?;
    source
        .catalog
        .drop_foreign_table(&row("removed").relation)?;
    let expected = source.catalog.load_foreign_tables()?;
    let control = StorageReadControl::with_limit(1 << 20);
    let mut input = source.catalog.begin_foreign_table_load(limits, &control)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later.catalog.drop_foreign_table(&row("z").relation)?;
    later.catalog.save_foreign_table(&row("later"))?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "foreign table input does not retain its source catalog handle",
    )?;
    let rows = finish(&mut input)?;
    expect(
        rows.len() == expected.len()
            && rows
                .iter()
                .zip(&expected)
                .all(|(actual, expected)| equal(actual, expected)),
        "foreign table input preserves original provider order, values and private membership",
    )?;
    expect(
        input.advance(1).is_err(),
        "a completed foreign table load cannot reopen its source",
    )?;
    control.cancellation().cancel();
    expect(
        rows.len() == expected.len()
            && rows
                .iter()
                .zip(&expected)
                .all(|(actual, expected)| equal(actual, expected)),
        "completed foreign tables survive original cancellation",
    )?;
    drop((rows, input));
    expect_eq(
        &control.memory().used(),
        &0,
        "foreign table retirement releases its complete input allowance",
    )?;
    verify_retirement(&later, limits)
}

fn verify_retirement(
    later: &PersistentStorageSession,
    limits: CatalogInputLimits,
) -> StorageBackendResult<()> {
    for terminal in ["cancel", "memory", "zero", "drop", "unwind", "encoded"] {
        let control = StorageReadControl::with_limit(1 << 20);
        let limits = if terminal == "encoded" {
            CatalogInputLimits {
                maximum_value_bytes: 1,
                ..limits
            }
        } else {
            limits
        };
        let mut input = later.catalog.begin_foreign_table_load(limits, &control)?;
        if terminal != "encoded" {
            expect(
                input.advance(1)?.is_none(),
                "one native poll cannot materialize the complete foreign table catalog",
            )?;
        }
        let occupied = if terminal == "memory" {
            Some(control.memory().reserve(control.memory().available())?)
        } else {
            None
        };
        match terminal {
            "cancel" => {
                control.cancellation().cancel();
                expect(
                    input.advance(1).is_err(),
                    "cancelled foreign table input fails",
                )?;
            }
            "zero" => expect(
                input.advance(0).is_err(),
                "empty foreign table window fails",
            )?,
            "memory" | "encoded" => expect(
                finish(&mut input).is_err(),
                "foreign table decoding respects original memory and encoded value limits",
            )?,
            "unwind" => {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _input = input;
                    panic!("abandon foreign table input");
                }));
                drop(occupied);
                expect_eq(
                    &control.memory().used(),
                    &0,
                    "unwinding retires foreign table input",
                )?;
                continue;
            }
            "drop" => (),
            _ => unreachable!(),
        }
        if terminal != "drop" {
            expect(
                input.advance(1).is_err(),
                "failed foreign table input stays terminal",
            )?;
        }
        drop((input, occupied));
        expect_eq(
            &control.memory().used(),
            &0,
            "partial foreign table input releases all private rows and cursor owners",
        )?;
    }
    Ok(())
}

fn row(name: &str) -> ForeignTableRow {
    ForeignTableRow {
        relation: RelationIdentity::new("foreign_input", name),
        security: RelationSecurityRow::bootstrap(),
        server_name: "original source".into(),
        columns_json: "original column metadata".into(),
        options_json: "{ \"key\": \"original\" }".into(),
    }
}

fn equal(left: &ForeignTableRow, right: &ForeignTableRow) -> bool {
    left.relation == right.relation
        && left.security == right.security
        && left.server_name == right.server_name
        && left.columns_json == right.columns_json
        && left.options_json == right.options_json
}
