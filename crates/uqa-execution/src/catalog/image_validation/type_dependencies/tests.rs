//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::catalog::{
    image_validation::tests::snapshot, test_support::table_snapshot, CatalogReadView,
};
use uqa_core::{
    catalog_role::RoleIdentity,
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::RetainedValue,
    RelationIdentity,
};
use uqa_sql::{
    ast::{ColumnDef, EnumTypeReference, TableConstraintSet},
    catalog::enum_type::StoredEnum,
    ColumnType,
};

const COUNT: u32 = 4096;

fn catalog(count: u32, valid: bool, width: u32) -> CatalogReadView {
    let mut catalog = snapshot();
    let enums = RetainedValue::make_mut(&mut catalog.definitions.enums);
    for index in 1..=count {
        let name = format!("m{index:05}");
        let oid = 20000 + index * 2;
        enums
            .insert(
                format!("public.{name}"),
                StoredEnum {
                    object_id: u128::from(index).to_be_bytes(),
                    oid,
                    array_oid: oid + 1,
                    array_name: format!("_{name}"),
                    identity: RelationIdentity::new("public", name),
                    owner: RoleIdentity::BOOTSTRAP,
                    labels: Vec::new(),
                    usage_acl: None,
                },
            )
            .unwrap();
    }
    let oid = 20000 + count * 2;
    let columns = (1..=width)
        .map(|index| {
            let ty = ColumnType::Enum(EnumTypeReference {
                schema: "public".into(),
                name: format!("m{count:05}"),
                oid,
                array_oid: if index == width && !valid { 0 } else { oid + 1 },
            });
            ColumnDef::nullable(format!("v{index:05}"), ty)
        })
        .collect();
    catalog.tables.insert(
        RelationIdentity::new("public", "typed"),
        table_snapshot([80; 16], columns, TableConstraintSet::default()),
    );
    CatalogReadView::new(catalog)
}

fn start(
    catalog: &CatalogReadView,
    memory: &MemoryBudget,
    cancellation: &CancellationToken,
) -> ImageDecoder<(), SQLError> {
    let catalog = catalog.clone();
    let original = cancellation.clone();
    ImageDecoder::new(memory, cancellation, move |mut reader| async move {
        from_input(catalog.snapshot(), &original, &mut reader.input()).await
    })
    .unwrap()
}

fn poll(input: &mut ImageDecoder<(), SQLError>, size: usize) -> Result<Option<()>, SQLError> {
    input.advance(
        &mut std::io::empty(),
        ImageDecodeWindow {
            bytes: 1,
            steps: size,
        },
    )
}

fn drain(catalog: &CatalogReadView, size: usize) -> (usize, Result<(), SQLError>) {
    let memory = MemoryBudget::new(8 << 20);
    let original = CancellationToken::new();
    let mut cursor = start(catalog, &memory, &original);
    for index in 1..1_000_000 {
        let result = match poll(&mut cursor, size) {
            Ok(None) => continue,
            Ok(Some(())) => Ok(()),
            Err(error) => Err(error),
        };
        assert_eq!(memory.used(), 0);
        return (index, result);
    }
    panic!("type closure did not terminate");
}

#[test]
fn type_claims_yield_before_checking_the_last_column_and_retire_on_every_exit() {
    let invalid = catalog(COUNT, false, 1);
    let original = CancellationToken::new();
    let memory = MemoryBudget::new(8 << 20);
    let inline = validate(invalid.snapshot(), &memory, &original)
        .unwrap_err()
        .to_string();
    assert!(inline.contains("enum array identity"), "{inline}");
    assert_eq!(memory.used(), 0);
    let (units, single) = drain(&invalid, 1);
    let (batches, batched) = drain(&invalid, 32);
    assert_eq!(single.unwrap_err().to_string(), inline);
    assert_eq!(batched.unwrap_err().to_string(), inline);
    assert!(units > COUNT as usize);
    assert_eq!(batches, units.div_ceil(32));
    for cancel in [false, true] {
        let token = CancellationToken::new();
        let mut cursor = start(&invalid, &memory, &token);
        for _ in 0..128 {
            assert!(poll(&mut cursor, 1).unwrap().is_none());
        }
        assert!(memory.used() > 0);
        if cancel {
            token.cancel();
            assert!(matches!(poll(&mut cursor, 1), Err(SQLError::Cancelled(_))));
        }
        drop(cursor);
        assert_eq!(memory.used(), 0);
    }
    let denied = MemoryBudget::new(64 << 10);
    let mut cursor = start(&invalid, &denied, &original);
    let mut pending = 0;
    loop {
        match poll(&mut cursor, 1) {
            Ok(None) => pending += 1,
            Err(error) => {
                assert_eq!(error.sqlstate(), Some("53200"));
                break;
            }
            Ok(Some(())) => panic!("over-budget type index was accepted"),
        }
    }
    assert!(pending > 1);
    assert_eq!(denied.used(), 0);
}

#[test]
fn type_membership_yields_for_each_edge_and_during_successful_claim_retirement() {
    let one = catalog(1, true, 1);
    let many = catalog(1, true, COUNT);
    let (few, result) = drain(&one, 1);
    result.unwrap();
    let (single, result) = drain(&many, 1);
    result.unwrap();
    let (batched, result) = drain(&many, 31);
    result.unwrap();
    assert!(single - few >= 2 * (COUNT as usize - 1));
    assert_eq!(batched, single.div_ceil(31));
    let large = catalog(COUNT, true, 1);
    let memory = MemoryBudget::new(8 << 20);
    let token = CancellationToken::new();
    let mut cursor = start(&large, &memory, &token);
    let mut polls_after_release = 0;
    while poll(&mut cursor, 1).unwrap().is_none() {
        if memory.peak() > (256 << 10) && memory.used() < memory.peak() / 2 {
            polls_after_release += 1;
        }
    }
    assert!(polls_after_release > COUNT as usize / 2);
    assert_eq!(memory.used(), 0);
}

#[test]
fn routine_utility_statements_keep_their_native_type_validation_order() {
    use uqa_sql::{
        ast::{FunctionBody, Statement},
        plan::UnifiedPlan,
        routines::{CompiledFunctionBody, RoutineBody, SQLUserFunction},
    };

    for (sql, expected) in [
        ("NOTIFY checked, 'result'", None),
        (
            "LISTEN checked",
            Some("unsupported stored command relation scope"),
        ),
    ] {
        let mut catalog = snapshot();
        let Statement::CreateFunction(mut function) = uqa_sql::compile(
            "CREATE FUNCTION public.checked() RETURNS void LANGUAGE SQL AS 'SELECT NULL'",
        )
        .unwrap()
        .remove(0) else {
            panic!("function declaration");
        };
        function.owner = Some(RoleIdentity::BOOTSTRAP);
        function.object_id = Some([33; 16]);
        let statements = uqa_sql::compile(sql).unwrap();
        let executable = statements.iter().cloned().map(UnifiedPlan::lower).collect();
        function.body = FunctionBody::Statements(statements);
        let function = SQLUserFunction::new(
            *function,
            RoutineBody::Bound(CompiledFunctionBody::SQL(executable).shared()),
        );
        RetainedValue::make_mut(&mut catalog.definitions.sql_user_functions)
            .insert("public.checked".into(), vec![function.shared()])
            .unwrap();
        let catalog = CatalogReadView::new(catalog);
        let memory = MemoryBudget::new(1 << 20);
        let inline = validate(catalog.snapshot(), &memory, &CancellationToken::new());
        let retained = drain(&catalog, 1).1;
        assert_eq!(
            retained.as_ref().err().map(ToString::to_string),
            inline.as_ref().err().map(ToString::to_string)
        );
        if let Some(expected) = expected {
            assert!(inline.unwrap_err().to_string().contains(expected));
        } else {
            inline.unwrap();
        }
        assert_eq!(memory.used(), 0);
    }
}

mod defaults;
mod plans;
