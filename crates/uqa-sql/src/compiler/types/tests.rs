//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn retained_type_names_equal_fresh_parses() {
    for spelling in [
        "integer",
        "pg_catalog.\"numeric\"",
        "public.\"Mixed Case\"[]",
        "character varying(25)",
        "int4 array",
        "timestamp(3) with time zone[][]",
        "   ",
        "setof integer",
    ] {
        let fresh = parse_regtype_name_text(spelling).unwrap();
        assert_eq!(parse_regtype_name(spelling).unwrap(), fresh, "{spelling:?}");
        assert_eq!(parse_regtype_name(spelling).unwrap(), fresh, "{spelling:?}");
    }
    assert!(parse_regtype_name("integer(").is_err());
}

#[test]
fn retained_type_names_stay_bounded() {
    for index in 0..=RETAINED_TYPE_NAMES {
        parse_regtype_name(&format!("bounded_type_{index}")).unwrap();
    }
    assert!(PARSED_TYPE_NAMES.read().unwrap().len() <= RETAINED_TYPE_NAMES);
}

mod production;
