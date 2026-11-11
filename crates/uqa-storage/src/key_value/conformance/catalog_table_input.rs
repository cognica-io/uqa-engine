//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Definition and replacement input keeps the original private catalog through source retirement.

use super::{expect, expect_eq};
use crate::{
    catalog::{
        input::CatalogInputLimits, relation_acl::RelationAclTuple,
        table_input::CatalogTableInputStep,
    },
    read_control::StorageReadControl,
    PersistentStorageSession, RelationIdentity, RelationSecurityRow, StorageBackendResult,
    StorageSavepointId, TableSchema,
};

/// Exercise a fresh disposable provider's complete physical table input, same-view ACLs, private rollback/name reuse and bounded unrelated metadata.
pub fn verify_catalog_table_input(source: PersistentStorageSession) -> StorageBackendResult<()> {
    let control = StorageReadControl::with_limit(65536);
    let limits = CatalogInputLimits {
        maximum_key_bytes: 4096,
        maximum_value_bytes: 8192,
    };
    let (mut input, expected, later) = prepare(source, limits, &control)?;
    verify_original(&mut input, &expected, &control)?;
    verify_denial(&later, limits)
}

type Prepared = (
    crate::catalog::table_input::CatalogTableInput,
    Vec<TableSchema>,
    PersistentStorageSession,
);

fn prepare(
    source: PersistentStorageSession,
    limits: CatalogInputLimits,
    control: &StorageReadControl,
) -> StorageBackendResult<Prepared> {
    source.catalog.initialize_storage()?;
    source.catalog.save_schema("table_input")?;
    let mut first = table("first", 1);
    source.catalog.save_table(&first)?;
    source.catalog.save_table(&table("deleted", 3))?;
    source
        .catalog
        .set_metadata("unrelated-large-table-input", &"x".repeat(4 << 20))?;
    source.backend.begin_transaction()?;
    let savepoint = StorageSavepointId::allocate();
    source.backend.savepoint(savepoint)?;
    first.analyzer_json = "private analyzer".into();
    source.catalog.save_table(&first)?;
    source.catalog.save_table(&table("private", 5))?;
    source.catalog.drop_table("table_input.deleted")?;
    let relation_acl = RelationAclTuple::new(Some(Vec::new()))?;
    let column_acl = RelationAclTuple::new(Some(Vec::new()))?;
    source
        .catalog
        .save_relation_acl(&first.relation, None, &relation_acl)?;
    source
        .catalog
        .save_relation_acl(&first.relation, Some("column"), &column_acl)?;
    let expected = source.catalog.load_tables()?;
    let input = source.catalog.begin_table_input(limits, control)?;
    source.backend.rollback_to_savepoint(savepoint)?;
    source.backend.rollback_transaction()?;
    let later = source.backend.open_session()?;
    later.catalog.drop_table_and_data("table_input.first")?;
    later.catalog.save_table(&table("first", 9))?;
    later.catalog.save_table(&table("private", 11))?;
    let owner = std::sync::Arc::downgrade(&source.catalog);
    drop(source);
    expect(
        owner.upgrade().is_none(),
        "table input does not retain the source catalog handle",
    )?;

    Ok((input, expected, later))
}

fn verify_original(
    input: &mut crate::catalog::table_input::CatalogTableInput,
    expected: &[TableSchema],
    control: &StorageReadControl,
) -> StorageBackendResult<()> {
    let mut definitions_complete = false;
    let mut rows = Vec::new();
    let mut replacements = Vec::new();
    let mut complete = false;
    for _ in 0..256 {
        match input.advance()? {
            CatalogTableInputStep::Pending => (),
            CatalogTableInputStep::Definition(row) => {
                expect(!definitions_complete, "definitions precede replacements")?;
                rows.push(row);
            }
            CatalogTableInputStep::DefinitionsComplete => {
                expect(!definitions_complete, "one definition boundary")?;
                definitions_complete = true;
            }
            CatalogTableInputStep::AclReplacement(row) => {
                expect(
                    definitions_complete,
                    "replacement follows original definitions",
                )?;
                replacements.push(row);
            }
            CatalogTableInputStep::Complete => {
                complete = true;
                break;
            }
        }
    }
    expect(
        complete && definitions_complete,
        "finite table input completes without replay",
    )?;
    expect_eq(
        &replacements.len(),
        &2,
        "original independent relation and column ACLs",
    )?;
    let mut actual: Vec<_> = rows.iter().map(|row| (***row).clone()).collect();
    for replacement in &replacements {
        for row in actual
            .iter_mut()
            .filter(|row| row.relation == replacement.relation)
        {
            let RelationSecurityRow::Bound(security) = &mut row.security else {
                return Err(crate::StorageBackendError::Other(
                    "bound table fixture".into(),
                ));
            };
            replacement
                .tuple
                .apply(replacement.column.as_deref(), security)?;
        }
    }
    actual.sort_by(|left, right| left.relation.cmp(&right.relation));
    expect_eq(
        &serde_json::to_value(&actual)?,
        &serde_json::to_value(expected)?,
        "original table fields and private membership",
    )?;
    for (actual, expected) in actual.iter().zip(expected) {
        expect_eq(
            &actual.security,
            &expected.security,
            "original inline and replacement authority revisions",
        )?;
    }
    let retained = rows
        .iter()
        .map(uqa_core::memory::Budgeted::reserved_bytes)
        .sum::<usize>()
        + replacements
            .iter()
            .map(uqa_core::memory::Budgeted::reserved_bytes)
            .sum::<usize>();
    expect_eq(
        &control.memory().used(),
        &retained,
        "completed input retains only returned native owners",
    )?;
    control.cancellation().cancel();
    expect(
        matches!(input.advance()?, CatalogTableInputStep::Complete),
        "completion remains idempotent after cancellation",
    )?;
    drop(rows);
    drop(replacements);
    expect_eq(
        &control.memory().used(),
        &0,
        "returned owners release their original allowances",
    )?;

    Ok(())
}

fn verify_denial(
    later: &PersistentStorageSession,
    limits: CatalogInputLimits,
) -> StorageBackendResult<()> {
    let denied = StorageReadControl::with_limit(65536);
    let mut input = later.catalog.begin_table_input(
        CatalogInputLimits {
            maximum_value_bytes: 1,
            ..limits
        },
        &denied,
    )?;
    let mut failed = false;
    for _ in 0..256 {
        match input.advance() {
            Err(_) => {
                failed = true;
                break;
            }
            Ok(CatalogTableInputStep::Complete) => break,
            Ok(_) => (),
        }
    }
    expect(
        failed,
        "encoded table value limit is enforced before decoding",
    )?;
    expect(input.advance().is_err(), "failed table input cannot resume")?;
    expect_eq(
        &denied.memory().used(),
        &0,
        "failed paired input releases both original streams",
    )?;
    Ok(())
}

fn table(name: &str, identity: u8) -> TableSchema {
    TableSchema {
        relation: RelationIdentity::new("table_input", name),
        security: RelationSecurityRow::bootstrap(),
        object_id: [identity; 16],
        storage_generation: [identity + 1; 16],
        analyzer_json: "original analyzer".into(),
        fts_fields: vec!["text".into(), "text".into(), "한🙂".into()],
        vector_fields: vec![crate::VectorFieldSchema {
            field: "vector".into(),
            dimensions: 3,
        }],
        columns_json: "original columns".into(),
        constraints_json: "original constraints".into(),
    }
}
