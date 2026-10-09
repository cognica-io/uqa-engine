//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::CompositeTypeReference, expr::composites::CompositeAttribute};

fn reference(oid: u32) -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "public".into(),
        name: format!("nested_{oid}"),
        oid,
        array_oid: oid + 1,
        relation_oid: oid - 1,
    })
}

fn descriptor(oid: u32, fields: &[(&str, ColumnType)]) -> Arc<CompositeTypeDescriptor> {
    Arc::new(CompositeTypeDescriptor {
        type_oid: oid,
        relation_oid: oid - 1,
        dropped: Vec::new(),
        attributes: fields
            .iter()
            .enumerate()
            .map(|(index, (name, ty))| CompositeAttribute {
                name: (*name).into(),
                ty: ty.clone(),
                number: index as i16 + 1,
            })
            .collect(),
    })
}

#[test]
fn nested_projection_and_binary_output_keep_the_original_inner_tuple() {
    let original = Descriptors::from([
        (
            20_000,
            descriptor(
                20_000,
                &[("a", ColumnType::Integer), ("b", ColumnType::Integer)],
            ),
        ),
        (
            20_010,
            descriptor(
                20_010,
                &[
                    ("lead", ColumnType::SmallInteger),
                    ("item", reference(20_000)),
                    ("tail", ColumnType::Integer),
                ],
            ),
        ),
    ]);
    let record = |a| {
        Value::Record(
            vec![
                ("lead".into(), Value::Int(9)),
                (
                    "item".into(),
                    Value::Record(vec![("a".into(), a), ("b".into(), Value::Int(17))].into()),
                ),
                ("tail".into(), Value::Int(23)),
            ]
            .into(),
        )
    };
    let source = record(Value::Int(1_065_353_216));
    let mut current = original.clone();
    Arc::make_mut(current.get_mut(&20_000).unwrap()).attributes[0].ty = ColumnType::Real;
    assert_eq!(
        project_value(&source, &reference(20_010), &original, &current, true).unwrap(),
        record(Value::Float(1.0))
    );

    Arc::make_mut(current.get_mut(&20_010).unwrap()).attributes[1].ty = ColumnType::Bytea;
    let Value::Record(fields) =
        project_value(&source, &reference(20_010), &original, &current, true).unwrap()
    else {
        panic!("outer record")
    };
    let Value::Datum(datum) = &fields[1].1 else {
        panic!("physical inner tuple")
    };
    // Independently captured PostgreSQL tuple payload; substitute only the fixture's type OID.
    let reference = "ffffffff204e0000ffffffff00000200000018000000803f11000000";
    let bytes = (0..reference.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&reference[index..index + 2], 16).unwrap())
        .collect();
    assert_eq!(
        crate::expr::datums::read(datum).unwrap(),
        Value::Bytes(bytes)
    );
    assert_eq!(fields[2].1, Value::Int(23));
    assert_eq!(source, record(Value::Int(1_065_353_216)));
}
