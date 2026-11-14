//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::image_io::{ImageDecodeWindow, ImageDecoder};

type Input = ImageDecoder<RoutineParameterScope, SQLError>;
type Columns = Arc<Vec<(String, Option<ColumnType>)>>;

fn input(
    function: Arc<String>,
    columns: Columns,
    native: &MemoryBudget,
    transport: &MemoryBudget,
    original: &CancellationToken,
    invoking: &CancellationToken,
) -> Input {
    let native = native.clone();
    let original = original.clone();
    ImageDecoder::new(transport, invoking, |mut reader| async move {
        let mut input = reader.input();
        let control = ProductionControl::new(&native, &original, input.cancellation);
        let mut selected = ProductionVec::new(control);
        selected.reserve(columns.len())?;
        for (name, ty) in columns.iter() {
            input.checkpoint().await?;
            selected.push_copy((name.as_str(), ty.as_ref()))?;
        }
        RoutineParameterScope::new_from_input(&function, &selected, &native, &original, &mut input)
            .await
    })
    .unwrap()
}

fn advance(reader: &mut Input) -> Result<Option<RoutineParameterScope>, SQLError> {
    reader.advance(
        &mut std::io::empty(),
        ImageDecodeWindow { bytes: 1, steps: 1 },
    )
}

fn complete(reader: &mut Input) -> (RoutineParameterScope, usize) {
    for polls in 1..100_000 {
        if let Some(scope) = advance(reader).unwrap() {
            return (scope, polls);
        }
    }
    panic!("parameter namespace stopped progressing")
}

#[test]
fn retained_parameter_scope_preserves_original_name_resolution_and_shares_its_final_owner() {
    let function = Arc::new("function.型".repeat(900));
    let columns = Arc::new(vec![
        ("argument_🦀".repeat(900), Some(ColumnType::Text)),
        ("".into(), Some(ColumnType::Integer)),
        ("quoted.column".into(), Some(ColumnType::Varchar(Some(17)))),
    ]);
    let expected = RoutineParameterScope::new(
        &function,
        columns.iter().map(|(name, _)| name.clone()).collect(),
        columns.iter().map(|(_, ty)| ty.clone()).collect(),
    );
    let native = MemoryBudget::new(8 << 20);
    let transport = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let source = Arc::downgrade(&columns);
    let mut reader = input(
        function.clone(),
        columns,
        &native,
        &transport,
        &token,
        &token,
    );
    let (scope, polls) = complete(&mut reader);
    assert!(polls > 30);
    assert!(source.upgrade().is_none());
    assert_eq!(scope.data.function, expected.data.function);
    assert_eq!(scope.data.names, expected.data.names);
    assert_eq!(scope.schema(), expected.schema());
    for name in &scope.data.names {
        let unqualified = crate::ScalarExpr::Column(name.clone().into());
        let qualified = crate::ScalarExpr::QualifiedColumn {
            qualifier: (*function).clone(),
            column: name.clone().into(),
        };
        assert_eq!(
            scope.parameter(&unqualified, scope.schema()),
            expected.parameter(&unqualified, expected.schema())
        );
        assert_eq!(
            scope.parameter(&qualified, scope.schema()),
            expected.parameter(&qualified, expected.schema())
        );
    }
    for qualifier in ["table", function.as_str()] {
        let local = RowSchema::with_qualified_types(
            qualifier,
            vec![scope.data.names[0].clone()],
            vec![Some(ColumnType::Boolean)],
        );
        let actual = RowSchema::with_outer_schema(&local, scope.schema());
        let reference = RowSchema::with_outer_schema(&local, expected.schema());
        assert_eq!(actual, reference);
        for name in &scope.data.names {
            let plain = crate::ScalarExpr::Column(name.clone().into());
            let qualified = crate::ScalarExpr::QualifiedColumn {
                qualifier: (*function).clone(),
                column: name.clone().into(),
            };
            assert_eq!(
                scope.parameter(&plain, &actual),
                expected.parameter(&plain, &reference)
            );
            assert_eq!(
                scope.parameter(&qualified, &actual),
                expected.parameter(&qualified, &reference)
            );
        }
    }
    let held = native.used();
    assert!(held > 0);
    assert_eq!(transport.used(), 0);
    let mut retained = None;
    let allocation = allocation_counter::measure(|| {
        retained = Some(scope.clone());
    });
    assert_eq!(allocation.count_total, 0);
    drop(scope);
    assert_eq!(native.used(), held);
    assert_eq!(retained.as_ref().unwrap().schema(), expected.schema());
    drop(retained);
    assert_eq!(native.used(), 0);
}

#[test]
fn retained_parameter_scope_retires_schema_and_name_prefixes_before_terminal_reuse() {
    let function = Arc::new(String::from("selected"));
    let columns = Arc::new(vec![("argument".into(), Some(ColumnType::Text))]);
    let native = MemoryBudget::new(1 << 20);
    let transport = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let (scope, polls) = complete(&mut input(
        function.clone(),
        columns.clone(),
        &native,
        &transport,
        &token,
        &token,
    ));
    drop(scope);
    let sibling = native.reserve(19).unwrap();
    for prefix in 0..polls - 1 {
        for outcome in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let mut reader = input(
                function.clone(),
                columns.clone(),
                &native,
                &transport,
                &original,
                &invoking,
            );
            for _ in 0..prefix {
                assert!(advance(&mut reader).unwrap().is_none());
            }
            if outcome != 0 {
                if outcome == 1 {
                    original.cancel();
                } else {
                    invoking.cancel();
                }
                assert_eq!(
                    advance(&mut reader).err().unwrap().sqlstate(),
                    Some("57014")
                );
                assert_eq!(
                    advance(&mut reader).err().unwrap().sqlstate(),
                    Some("XX001")
                );
            }
            drop(reader);
            assert_eq!((native.used(), transport.used()), (19, 0));
        }
    }
    drop(sibling);
}
