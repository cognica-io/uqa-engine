//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeMap;

use uqa_sql::binding::stored_types::TypeNameSite;

use super::UserTypeNames;

fn public() -> Vec<String> {
    vec!["public".to_string()]
}

fn names() -> UserTypeNames {
    UserTypeNames {
        identities: BTreeMap::from([
            (("public".into(), "positive".into()), "domain#16390".into()),
            (("sales".into(), "Amount".into()), "domain#16400".into()),
        ]),
    }
}

#[test]
fn stored_names_resolve_as_the_default_search_path_found_them() {
    let names = names();
    assert_eq!(
        names
            .identity("positive", TypeNameSite::Written, &public())
            .as_deref(),
        Some("domain#16390")
    );
    assert_eq!(
        names
            .identity("public.positive", TypeNameSite::Written, &public())
            .as_deref(),
        Some("domain#16390")
    );
    assert_eq!(
        names
            .identity("positive[]", TypeNameSite::Written, &public())
            .as_deref(),
        Some("domain#16390[]")
    );
    assert_eq!(
        names
            .identity("positive[][]", TypeNameSite::Written, &public())
            .as_deref(),
        Some("domain#16390[][]")
    );
    assert_eq!(
        names
            .identity("sales.\"Amount\"", TypeNameSite::Written, &public())
            .as_deref(),
        Some("domain#16400")
    );
}

#[test]
fn other_names_are_left_alone() {
    let names = names();
    // An unqualified name outside the default schema, a differently cased quoted name and unknown names do not resolve.
    assert_eq!(
        names.identity("\"Amount\"", TypeNameSite::Written, &public()),
        None
    );
    assert_eq!(
        names.identity("\"Positive\"", TypeNameSite::Written, &public()),
        None
    );
    assert_eq!(
        names.identity("sales.amount", TypeNameSite::Written, &public()),
        None
    );
    assert_eq!(
        names.identity("missing", TypeNameSite::Written, &public()),
        None
    );
    assert_eq!(
        names.identity("a.b.c", TypeNameSite::Written, &public()),
        None
    );
}

#[test]
fn search_paths_and_canonical_spellings_resolve() {
    let names = names();
    let sales = vec!["sales".to_string(), "public".to_string()];
    assert_eq!(
        names
            .identity("\"Amount\"", TypeNameSite::Written, &sales)
            .as_deref(),
        Some("domain#16400")
    );
    // Routine bindings fold quoted names to lower case.
    assert_eq!(
        names
            .identity("sales.\"amount\"", TypeNameSite::Canonical, &public())
            .as_deref(),
        Some("domain#16400")
    );
    assert_eq!(
        names.identity("sales.\"amount\"", TypeNameSite::Written, &public()),
        None
    );
}
