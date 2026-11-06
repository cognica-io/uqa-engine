//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::super::validation::{malformed, rejects};
use super::*;

#[test]
fn structured_type_images_reject_all_truncations_bad_counts_and_duplicate_keys() {
    malformed(&singleton("public.c", composite()));
    malformed(&singleton("public.r", range()));
}

#[test]
fn composite_images_reject_invalid_identity_attribute_order_and_unresolved_types() {
    for corruption in 0..12 {
        let mut value = composite();
        match corruption {
            0 => value.object_id = [0; 16],
            1 => value.oid = 0,
            2 => value.relation_oid = value.array_oid,
            3 => value.array_name.clear(),
            4 => value.identity.name = "other".into(),
            5 => value.owner.object_id = [0; 16],
            6 => value.attributes[0].number = 0,
            7 => value.attributes.push(value.attributes[0].clone()),
            8 => {
                let mut second = value.attributes[0].clone();
                second.number = 4;
                value.attributes.push(second);
            }
            9 => value.attributes[0].ty = ColumnType::Named("integer".into()),
            10 => {
                value.attributes[0].ty =
                    ColumnType::Array(Box::new(ColumnType::Named("integer".into())));
            }
            _ => value.attributes[0].name.clear(),
        }
        rejects::<StoredComposite>(&encode(&singleton("public.c", value)));
    }
    let bytes = encode(&singleton("public.c", composite()));
    for tag in [38, 59, 255] {
        let mut invalid = bytes.clone();
        *invalid.last_mut().unwrap() = tag;
        rejects::<StoredComposite>(&invalid);
    }
}

#[test]
fn range_images_reject_identity_aliases_support_errors_and_companion_name_collisions() {
    for corruption in 0..12 {
        let mut value = range();
        match corruption {
            0 => value.multirange.object_id = value.object_id,
            1 => value.multirange.oid = value.oid,
            2 => value.multirange.array_oid = value.array_oid,
            3 => value.multirange.array_name.clear(),
            4 => value.multirange.identity = value.identity.clone(),
            5 => value.subtype_operator_class = 0,
            6 => value.canonical_oid = Some(0),
            7 => value.subtype_difference_oid = Some(0),
            8 => value.subtype = ColumnType::Named("integer".into()),
            9 => value.array_name = value.identity.name.clone(),
            10 => value.multirange.identity.schema = "pg_temp_123".into(),
            _ => value.owner.object_id = [0; 16],
        }
        rejects::<StoredRange>(&encode(&singleton("public.r", value)));
    }
}

#[test]
fn structured_type_images_reject_invalid_nested_tags_options_and_field_masks() {
    let image = encode(&singleton("public.c", composite()));
    let prefix = &image[..image.len() - 1];
    for node in [
        &[44, 255][..],
        &[39, 2],
        &[41, 0, 2],
        &[49, 0, 0, 0],
        &[50, 6],
        &[51, 6],
        &[44],
        &[44, 0, 0],
    ] {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(node);
        rejects::<StoredComposite>(&bytes);
    }
    let mut bytes = prefix.to_vec();
    bytes.extend(std::iter::repeat_n(44, 256));
    bytes.push(1);
    let token = CancellationToken::new();
    let memory = MemoryBudget::new(128);
    assert_eq!(
        StoredComposite::decode(&mut bytes.as_slice(), &memory, &token)
            .err()
            .unwrap()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
}
