//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::RefCell;

struct Session {
    path: Vec<String>,
    reads: RefCell<Vec<&'static str>>,
}
impl Session {
    fn new(path: &[&str]) -> Self {
        Self {
            path: path.iter().map(|schema| (*schema).into()).collect(),
            reads: RefCell::new(Vec::new()),
        }
    }
}
impl RelationCandidateState for Session {
    fn temporary_schema_name(&self) -> String {
        self.reads.borrow_mut().push("temporary");
        "pg_temp_42".into()
    }
    fn search_path(&self) -> SearchPathRead<'_> {
        self.reads.borrow_mut().push("search_path");
        Box::new(&self.path)
    }
}

#[test]
fn qualified_names_preserve_quoted_components_without_session_reads() {
    let session = Session::new(&["ignored"]);
    assert_eq!(
        relation_lookup_candidates(&session, "public.items").unwrap(),
        vec![RelationIdentity::new("public", "items")]
    );
    assert_eq!(
        relation_lookup_candidates(&session, r#""schema.with.dot"."item""quote""#).unwrap(),
        vec![RelationIdentity::new("schema.with.dot", "item\"quote")]
    );
    assert!(session.reads.borrow().is_empty());
}

#[test]
fn temporary_alias_reads_only_the_session_namespace() {
    let session = Session::new(&["ignored"]);
    assert_eq!(
        relation_lookup_candidates(&session, r#"pg_temp."a.b""#).unwrap(),
        vec![RelationIdentity::new("pg_temp_42", "a.b")]
    );
    assert_eq!(*session.reads.borrow(), vec!["temporary"]);
}

#[test]
fn unqualified_names_preserve_search_order_and_duplicates_after_temporary_schema() {
    let session = Session::new(&[
        "tenant",
        "pg_catalog",
        "public",
        "information_schema",
        "tenant",
    ]);
    assert_eq!(
        relation_lookup_candidates(&session, "items").unwrap(),
        ["pg_temp_42", "tenant", "public", "tenant"]
            .into_iter()
            .map(|schema| RelationIdentity::new(schema, "items"))
            .collect::<Vec<_>>()
    );
    assert_eq!(*session.reads.borrow(), vec!["temporary", "search_path"]);
    let empty = Session::new(&[]);
    assert_eq!(
        relation_lookup_candidates(&empty, "items").unwrap(),
        vec![RelationIdentity::new("pg_temp_42", "items")]
    );
}

#[test]
fn malformed_references_fail_before_reading_session_state() {
    let session = Session::new(&["public"]);
    for name in ["", "\"unterminated", "a.b.c"] {
        let error = relation_lookup_candidates(&session, name).unwrap_err();
        assert_eq!(error, RelationIdentity::parse_reference(name).unwrap_err());
    }
    assert!(session.reads.borrow().is_empty());
}
