//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::catalog::{
    projection::helpers::index_definitions::index_key_definition, test_support::empty_catalog,
    RelationLookupMode, RelationNameResolution,
};

#[test]
fn named_call_reconstruction_preserves_notation_and_keyword_quoting() {
    let catalog = empty_catalog();
    let resolution = RelationNameResolution {
        search_path: vec!["public".into()],
        temporary_schema: "pg_temp_1".into(),
        temporary_namespace_allocated: false,
        current_user: "uqa".into(),
        lookup_mode: RelationLookupMode::Dynamic,
    };
    for (input, expected) in [
        (
            "array_sort(descending => true, \"array\" => ARRAY[2,1])",
            "array_sort(descending => true, \"array\" => ARRAY[2, 1])",
        ),
        (
            "array_sort(value => ARRAY[2,1])",
            "array_sort(value => ARRAY[2, 1])",
        ),
    ] {
        let uqa_sql::Statement::CreateIndex(index) =
            uqa_sql::compile(&format!("CREATE INDEX probe ON items (({input}))"))
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        for pretty in [false, true] {
            assert_eq!(
                index_key_definition(&catalog, &resolution, &index.columns[0], pretty).unwrap(),
                expected,
            );
        }
    }
}
