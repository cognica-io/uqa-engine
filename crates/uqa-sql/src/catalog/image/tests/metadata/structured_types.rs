//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{
        ColumnType, CompositeTypeReference, EnumTypeReference, IntervalFields, RangeAlignment,
        RangeSubtype, RangeTypeReference,
    },
    catalog::{
        composite_type::{StoredComposite, StoredCompositeAttribute},
        range_type::{StoredMultirange, StoredRange},
    },
};

mod validation;
mod wire;

fn attribute(name: &str, number: i16, ty: ColumnType) -> StoredCompositeAttribute {
    StoredCompositeAttribute {
        name: name.into(),
        ty,
        collation: None,
        collation_oid: None,
        number,
        dropped: false,
    }
}
fn composite() -> StoredComposite {
    StoredComposite {
        object_id: [10; 16],
        oid: 65536,
        relation_oid: 65537,
        array_oid: 65538,
        array_name: "_c".into(),
        identity: RelationIdentity::new("public", "c"),
        owner: RoleIdentity::BOOTSTRAP,
        attributes: vec![attribute("a", 3, ColumnType::Integer)],
        usage_acl: None,
    }
}
fn range() -> StoredRange {
    StoredRange {
        object_id: [11; 16],
        oid: 65539,
        array_oid: 65540,
        array_name: "_r".into(),
        identity: RelationIdentity::new("public", "r"),
        owner: RoleIdentity::BOOTSTRAP,
        subtype: ColumnType::Integer,
        subtype_operator_class: 1978,
        collation_oid: 0,
        canonical_oid: None,
        subtype_difference_oid: None,
        multirange: StoredMultirange {
            object_id: [12; 16],
            oid: 65541,
            array_oid: 65542,
            array_name: "_m".into(),
            identity: RelationIdentity::new("moved", "m"),
        },
        usage_acl: Some(vec![]),
    }
}
fn base_types() -> Vec<ColumnType> {
    use ColumnType::*;
    vec![
        SmallInteger,
        Integer,
        BigInteger,
        Oid,
        Xid,
        Boolean,
        Void,
        Text,
        RefCursor,
        Name,
        Uuid,
        Bpchar,
        Real,
        DoublePrecision,
        Json,
        Xml,
        JsonB,
        Bytea,
        InternalChar,
        Regproc,
        Regprocedure,
        Regclass,
        Regnamespace,
        Regcollation,
        Regrole,
        Regtype,
        PgNodeTree,
        AclItem,
        Int2Vector,
        OidVector,
        AnyArray,
        Record,
        Date,
        Time,
        TimeTz,
        Timestamp,
        TimestampTz,
        Interval,
        Character(7),
        TimePrecision(0),
        TimeTzPrecision(6),
        TimestampPrecision(3),
        TimestampTzPrecision(4),
        Vector(0),
        Tensor(17),
        Enum(EnumTypeReference {
            schema: "Old.Schema".into(),
            name: "e".into(),
            oid: 70000,
            array_oid: 70001,
        }),
        Composite(CompositeTypeReference {
            schema: "Old.Schema".into(),
            name: "c".into(),
            oid: 70002,
            array_oid: 70003,
            relation_oid: 70004,
        }),
        Array(Box::new(Domain {
            schema: "Old.Schema".into(),
            name: "d".into(),
            oid: 70005,
            array_oid: None,
            collation_oid: Some(0),
            base: Box::new(Numeric {
                precision: Some(1000),
                scale: Some(-1000),
            }),
        })),
    ]
}

fn types() -> Vec<ColumnType> {
    use ColumnType::*;
    let mut values = base_types();
    for length in [None, Some(0), Some(13), Some(u32::MAX)] {
        values.extend([Varchar(length), Bit(length), VarBit(length)]);
    }
    for precision in [None, Some(3)] {
        for scale in [None, Some(-2), Some(5)] {
            values.push(Numeric { precision, scale });
        }
    }
    for fields in [
        IntervalFields::All,
        IntervalFields::Year,
        IntervalFields::Month,
        IntervalFields::Day,
        IntervalFields::Hour,
        IntervalFields::Minute,
        IntervalFields::Second,
        IntervalFields::YearToMonth,
        IntervalFields::DayToHour,
        IntervalFields::DayToMinute,
        IntervalFields::DayToSecond,
        IntervalFields::HourToMinute,
        IntervalFields::HourToSecond,
        IntervalFields::MinuteToSecond,
    ] {
        for precision in [None, Some(0), Some(6)] {
            values.push(IntervalWithFields { fields, precision });
        }
    }
    for subtype in [
        RangeSubtype::Integer,
        RangeSubtype::BigInteger,
        RangeSubtype::Numeric,
        RangeSubtype::Date,
        RangeSubtype::Timestamp,
        RangeSubtype::TimestampTz,
    ] {
        values.extend([Range(subtype), Multirange(subtype)]);
    }
    for alignment in [RangeAlignment::Integer, RangeAlignment::Double] {
        let reference = RangeTypeReference {
            schema: "old".into(),
            name: "r".into(),
            oid: 70006,
            array_oid: 70007,
            alignment,
        };
        values.extend([
            DeclaredRange(reference.clone()),
            DeclaredMultirange(reference),
        ]);
    }
    values.push(Domain {
        schema: "public".into(),
        name: "d".into(),
        oid: 70005,
        array_oid: Some(70008),
        collation_oid: None,
        base: Box::new(Text),
    });
    values
}

#[test]
fn composite_images_preserve_every_native_type_modifier_reference_and_dropped_attribute() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8 << 20);
    let mut value = composite();
    value.attributes = types()
        .into_iter()
        .enumerate()
        .map(|(i, ty)| attribute(&format!("a{i}"), i as i16 + 1, ty))
        .collect();
    value.attributes[0].dropped = true;
    value.attributes[0].name = "same_name".into();
    value.attributes[1].name = "same_name".into();
    value.attributes[1].collation = Some("pg_catalog.\"C\"".into());
    value.attributes[1].collation_oid = Some(950);
    value.usage_acl = Some(acl());
    let values = singleton("public.c", value);
    let bytes = encode(&values);
    assert_round_trip(&values, &bytes);
    let restored = StoredComposite::decode(&mut bytes.as_slice(), &memory, &token).unwrap();
    // Native reference equality compares only OIDs; compare every serialized field independently.
    assert_eq!(
        serde_json::to_value(&values).unwrap(),
        serde_json::to_value(&restored).unwrap()
    );
    assert_eq!(restored["public.c"].attributes[0].number, 1);
    assert!(restored["public.c"].attributes[0].dropped);
    assert_eq!(restored["public.c"].attributes[1].collation_oid, Some(950));
    drop(restored);
    assert_eq!(memory.used(), 0);
}

#[test]
fn range_images_preserve_moved_multiranges_selected_support_and_acl_paths() {
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(8 << 20);
    for acl in [None, Some(vec![]), Some(acl())] {
        let mut value = range();
        value.usage_acl = acl;
        value.canonical_oid = Some(80001);
        value.subtype_difference_oid = Some(80002);
        value.collation_oid = 950;
        value.subtype = ColumnType::Domain {
            schema: "original".into(),
            name: "endpoint".into(),
            oid: 80003,
            array_oid: None,
            collation_oid: Some(950),
            base: Box::new(ColumnType::Text),
        };
        let values = singleton("public.r", value);
        let bytes = encode(&values);
        assert_round_trip(&values, &bytes);
        let restored = StoredRange::decode(&mut bytes.as_slice(), &memory, &token).unwrap();
        assert_eq!(
            serde_json::to_value(&values).unwrap(),
            serde_json::to_value(&restored).unwrap()
        );
        assert_eq!(restored["public.r"].multirange.identity.schema, "moved");
        assert_eq!(restored["public.r"].canonical_oid, Some(80001));
        drop(restored);
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn structured_type_images_retain_exact_native_admission_after_clone_and_capture() {
    let mut value = composite();
    value.attributes = types()
        .into_iter()
        .enumerate()
        .map(|(i, ty)| attribute(&format!("a{i}"), i as i16 + 1, ty))
        .collect();
    super::retention::retained(&singleton("public.c", value));
    super::retention::retained(&singleton("public.r", range()));
}

#[test]
fn structured_type_image_readers_bound_nested_types_and_large_attributes_without_allocation() {
    let mut value = composite();
    value.attributes = (1..=4096)
        .map(|number| attribute(&format!("a{number}"), number, ColumnType::Text))
        .collect();
    for _ in 0..256 {
        value.attributes[0].ty = ColumnType::Array(Box::new(value.attributes[0].ty.clone()));
    }
    super::retention::bounded(&singleton("public.c", value));
    let mut value = range();
    value.usage_acl = Some(acl().into_iter().cycle().take(4096).collect());
    super::retention::bounded(&singleton("public.r", value));
}

#[test]
fn cancelled_structured_type_import_retires_partial_boxed_types_and_attributes() {
    let mut value = composite();
    value.attributes = types()
        .into_iter()
        .enumerate()
        .map(|(i, ty)| attribute(&format!("a{i}"), i as i16 + 1, ty))
        .collect();
    super::retention::cancellation(&singleton("public.c", value));
    super::retention::cancellation(&singleton("public.r", range()));
}
