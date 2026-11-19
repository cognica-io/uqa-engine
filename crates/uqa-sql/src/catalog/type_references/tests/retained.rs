//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{Expr, RetainedInputLiteral},
    ir::{RetainedConstantValue, RetainedScalarLiteral},
    ScalarExpr,
};

fn image(mut reader: crate::catalog::image::DefinitionImageReader<'_>) -> Vec<u8> {
    let mut output = Vec::new();
    let mut buffer = [0; 127];
    loop {
        let count = reader.read(&mut buffer).unwrap();
        if count == 0 {
            return output;
        }
        output.extend_from_slice(&buffer[..count]);
    }
}

#[test]
fn retained_literal_type_references_survive_native_images_and_reject_missing_claims() {
    let types = Types::with_enum();
    let memory = MemoryBudget::new(1 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let index = TypeReferences::new(types.input(), std::iter::empty(), &memory, &token).unwrap();
    let retained = memory.used();
    for missing in [None, Some("type"), Some("declaration")] {
        {
            let mut ty = mood();
            if missing == Some("type") {
                let ColumnType::Enum(reference) = &mut ty else {
                    unreachable!()
                };
                reference.oid = 20_002;
            }
            let value = RetainedConstantValue::from_produced(
                control.copy_value(&uqa_core::Value::Null).unwrap(),
                &control,
            )
            .unwrap();
            let literal = RetainedScalarLiteral::with_control(
                value,
                Some(ty.clone_with_control(&control).unwrap()),
                Some(
                    control
                        .copy_text(if missing == Some("declaration") {
                            "enum#20002"
                        } else {
                            "enum#20000"
                        })
                        .unwrap(),
                ),
                None,
                &control,
            )
            .unwrap();
            let syntax = Expr::RetainedLiteral(Box::new(RetainedInputLiteral(literal.clone())));
            let scalar = ScalarExpr::RetainedLiteral(literal);
            let syntax_bytes = image(syntax.image_reader(&memory, &token).unwrap());
            let scalar_bytes = image(scalar.image_reader(&memory, &token).unwrap());
            let decoded_syntax =
                Expr::read_image(&mut syntax_bytes.as_slice(), &memory, &token).unwrap();
            let decoded_scalar =
                ScalarExpr::read_image(&mut scalar_bytes.as_slice(), &memory, &token).unwrap();
            for original in [&syntax, &*decoded_syntax] {
                assert_eq!(
                    index.expression(original).is_ok(),
                    missing.is_none(),
                    "syntax {missing:?}"
                );
            }
            for original in [&scalar, &*decoded_scalar] {
                assert_eq!(
                    index.scalar(original).is_ok(),
                    missing.is_none(),
                    "scalar {missing:?}"
                );
            }
            assert_eq!(
                image(decoded_syntax.image_reader(&memory, &token).unwrap()),
                syntax_bytes
            );
            assert_eq!(
                image(decoded_scalar.image_reader(&memory, &token).unwrap()),
                scalar_bytes
            );
        }
        assert_eq!(memory.used(), retained);
    }
    drop(index);
    assert_eq!(memory.used(), 0);
}
