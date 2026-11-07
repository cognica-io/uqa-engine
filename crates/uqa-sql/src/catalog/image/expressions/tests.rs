//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{BinaryOp, FunctionCallSyntax, InternalColumnRef, NullsOrder};
use uqa_core::value_image::write_value_image;

mod domains;
mod metadata;
mod retention;

fn collect(mut reader: DefinitionImageReader<'_>, width: usize) -> Vec<u8> {
    let mut output = vec![];
    let mut buffer = vec![0; width];
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    loop {
        let count = reader.read(&mut buffer).unwrap();
        assert!(count <= WINDOW);
        if count == 0 {
            break;
        }
        output.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(reader.read(&mut buffer).unwrap(), 0);
    output
}

fn encode(value: &Expr) -> Vec<u8> {
    let memory = MemoryBudget::new(16 << 20);
    let token = CancellationToken::new();
    let result = collect(value.image_reader(&memory, &token).unwrap(), 17);
    assert_eq!(memory.used(), 0);
    result
}

fn image(payload: &[u8]) -> Vec<u8> {
    let mut result = b"UQAEXP01\x01\0\0\0\0\0\0\0".to_vec();
    result.extend_from_slice(payload);
    result
}

fn text(value: &str) -> Vec<u8> {
    let mut result = (value.len() as u64).to_le_bytes().to_vec();
    result.extend_from_slice(value.as_bytes());
    result
}

fn value_image(value: &Value) -> Vec<u8> {
    let mut result = vec![];
    write_value_image(
        value,
        &mut result,
        &MemoryBudget::new(16 << 20),
        &CancellationToken::new(),
    )
    .unwrap();
    result
}

#[test]
fn scalar_leaf_images_match_literal_tags_and_preserve_original_payloads() {
    for (value, expected) in [(Expr::Star, vec![0]), (Expr::Default, vec![2])] {
        assert_eq!(encode(&value), image(&expected));
    }
    for (tag, value) in [
        (1, Expr::QualifiedStar("a.\"b\"".into())),
        (3, Expr::Column("a.\"b\"".into())),
    ] {
        let mut expected = vec![tag];
        expected.extend(text("a.\"b\""));
        assert_eq!(encode(&value), image(&expected));
    }
    let value = Expr::QualifiedColumn {
        qualifier: "old.schema".into(),
        column: crate::ast::ReferenceName::from("c").with_position(Some(91)),
    };
    let mut expected = vec![4];
    expected.extend(text("old.schema"));
    expected.extend(text("c"));
    assert_eq!(encode(&value), image(&expected));
    let value = Expr::InternalColumn(InternalColumnRef::from_raw(42, 17));
    let mut expected = vec![5];
    expected.extend(42u64.to_le_bytes());
    expected.extend(17u32.to_le_bytes());
    assert_eq!(encode(&value), image(&expected));
    let mut expected = vec![8];
    expected.extend((usize::MAX as u64).to_le_bytes());
    assert_eq!(encode(&Expr::Param(usize::MAX)), image(&expected));
    for value in [
        Value::Null,
        Value::Bool(true),
        Value::Int(i64::MIN),
        Value::Float(f64::from_bits(0x7ff8_0000_0000_1234)),
        Value::Str("α\0original".into()),
    ] {
        let mut expected = vec![6];
        expected.extend(value_image(&value));
        assert_eq!(encode(&Expr::Literal(value)), image(&expected));
    }
}

#[test]
fn scalar_container_images_preserve_child_order_and_written_flags() {
    for (tag, value) in [
        (10, Expr::Array(vec![Expr::Star, Expr::Default])),
        (11, Expr::Row(vec![Expr::Star, Expr::Default])),
        (16, Expr::And(vec![Expr::Star, Expr::Default])),
        (17, Expr::Or(vec![Expr::Star, Expr::Default])),
    ] {
        let mut expected = vec![tag];
        expected.extend(2u64.to_le_bytes());
        expected.extend([0, 2]);
        assert_eq!(encode(&value), image(&expected));
    }
    use BinaryOp as B;
    for (tag, op) in [
        B::Equal,
        B::NotEqual,
        B::Less,
        B::LessEqual,
        B::Greater,
        B::GreaterEqual,
        B::Add,
        B::Subtract,
        B::Multiply,
        B::Divide,
    ]
    .into_iter()
    .enumerate()
    {
        let value = Expr::Binary {
            op,
            lhs: Box::new(Expr::Star),
            rhs: Box::new(Expr::Default),
        };
        assert_eq!(encode(&value), image(&[13, tag as u8, 0, 2]));
    }
    assert_eq!(
        encode(&Expr::UnaryMinus(Box::new(Expr::Star))),
        image(&[14, 0])
    );
    assert_eq!(encode(&Expr::Not(Box::new(Expr::Star))), image(&[15, 0]));
    for negated in [false, true] {
        assert_eq!(
            encode(&Expr::IsNull {
                expr: Box::new(Expr::Default),
                negated
            }),
            image(&[18, u8::from(negated), 2])
        );
        let value = Expr::InList {
            expr: Box::new(Expr::Default),
            list: vec![Expr::Star, Expr::Default],
            negated,
        };
        let mut expected = vec![20, u8::from(negated)];
        expected.extend(2u64.to_le_bytes());
        expected.extend([2, 0, 2]);
        assert_eq!(encode(&value), image(&expected));
    }
    let value = Expr::Between {
        expr: Box::new(Expr::Star),
        low: Box::new(Expr::Default),
        high: Box::new(Expr::Star),
    };
    assert_eq!(encode(&value), image(&[19, 0, 2, 0]));
    let value = Expr::Case {
        base: Some(Box::new(Expr::Star)),
        when: vec![(Expr::Default, Expr::Star), (Expr::Star, Expr::Default)],
        else_branch: Some(Box::new(Expr::Default)),
    };
    let mut expected = vec![21, 1];
    expected.extend(2u64.to_le_bytes());
    expected.extend([1, 0, 2, 0, 0, 2, 2]);
    assert_eq!(encode(&value), image(&expected));
    for implicit in [false, true] {
        let value = Expr::Cast {
            implicit,
            expr: Box::new(Expr::Default),
            ty: "original.type[]".into(),
        };
        let mut expected = vec![22, u8::from(implicit)];
        expected.extend(text("original.type[]"));
        expected.push(2);
        assert_eq!(encode(&value), image(&expected));
    }
}

#[test]
fn function_expression_images_keep_binding_arguments_order_and_filter_separate() {
    let token = CancellationToken::new();
    let binding = FunctionBinding {
        object_id: Some([9; 16]),
        name: "old.f".into(),
        argument_types: vec!["text".into()],
        builtin: false,
        dispatch: Some(crate::ast::FunctionDispatch::ArrayConcat),
        invocation: None,
        value_binding: None,
        resolution_error: None,
        input_collation: None,
    };
    for (syntax, tag) in [
        (FunctionCallSyntax::Legacy, 0),
        (FunctionCallSyntax::Ordinary, 1),
        (FunctionCallSyntax::WithinGroup, 2),
        (FunctionCallSyntax::Extract, 3),
    ] {
        let value = Expr::Func {
            name: "original.f".into(),
            binding: Some(binding.clone()),
            args: vec![Expr::Star, Expr::Default],
            distinct: true,
            order_by: vec![
                OrderBy {
                    expr: Expr::Star,
                    descending: true,
                    nulls: None,
                },
                OrderBy {
                    expr: Expr::Default,
                    descending: false,
                    nulls: Some(NullsOrder::First),
                },
                OrderBy {
                    expr: Expr::Star,
                    descending: false,
                    nulls: Some(NullsOrder::Last),
                },
            ],
            order_syntax: syntax,
            filter: Some(Box::new(Expr::Default)),
        };
        let mut expected = vec![9];
        expected.extend(text("original.f"));
        expected.extend([1, 1, tag]);
        expected.extend(2u64.to_le_bytes());
        expected.extend(3u64.to_le_bytes());
        expected.push(1);
        let mut reader = binding.image_reader(&token).unwrap();
        let mut block = [0; 17];
        loop {
            let count = reader.read(&mut block).unwrap();
            if count == 0 {
                break;
            }
            expected.extend_from_slice(&block[..count]);
        }
        expected.extend([0, 2, 1, 0, 0, 0, 1, 2, 0, 2, 0, 2]);
        assert_eq!(encode(&value), image(&expected));
        for width in [1, 7, 4096, 20000] {
            let memory = MemoryBudget::new(16 << 20);
            assert_eq!(
                collect(value.image_reader(&memory, &token).unwrap(), width),
                image(&expected)
            );
            assert_eq!(memory.used(), 0);
        }
    }
}
