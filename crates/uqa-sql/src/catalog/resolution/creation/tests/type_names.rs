//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct Names {
    kind: &'static str,
    identity: RelationIdentity,
}
struct Present(bool);
impl CreationRelationNames for Present {
    fn contains(&self, _: &RelationIdentity) -> bool {
        self.0
    }
}
impl Names {
    fn matches(&self, kind: &str) -> Box<dyn CreationRelationNames> {
        Box::new(Present(self.kind == kind))
    }
}
impl CreationRelationGuards for Names {
    fn named_type_exists(&self, identity: &RelationIdentity) -> bool {
        self.kind == "domain" && identity == &self.identity
    }
    fn tables(&self) -> Box<dyn CreationRelationNames + '_> {
        self.matches("table")
    }
    fn views(&self) -> Box<dyn CreationRelationNames + '_> {
        self.matches("view")
    }
    fn sequences(&self) -> Box<dyn CreationRelationNames + '_> {
        self.matches("sequence")
    }
    fn foreign_tables(&self) -> Box<dyn CreationRelationNames + '_> {
        self.matches("foreign")
    }
    fn indexes(&self) -> Box<dyn CreationRelationNames + '_> {
        self.matches("index")
    }
}

#[test]
fn type_names_include_domains_and_row_types_but_not_sequences_or_indexes() {
    for kind in ["domain", "table", "view", "foreign", "sequence", "index"] {
        let catalog = Names {
            kind,
            identity: RelationIdentity::new("other", "Mixed Name"),
        };
        let type_collision = !matches!(kind, "sequence" | "index");
        assert_eq!(
            type_name_in_use(&catalog, &catalog.identity),
            type_collision
        );
        assert_eq!(
            relation_name_in_use(&catalog, &catalog.identity),
            kind != "domain"
        );
        let result = ensure_type_name_available(&catalog, &catalog.identity);
        if type_collision {
            let error = result.unwrap_err();
            assert_eq!(error.sqlstate(), Some("42710"));
            assert_eq!(error.to_string(), "type \"Mixed Name\" already exists");
        } else {
            result.unwrap();
        }
    }
}

#[test]
fn domain_type_collisions_preserve_qualified_names_and_quoted_case() {
    let catalog = Names {
        kind: "domain",
        identity: RelationIdentity::new("other", "Mixed Name"),
    };
    for identity in [
        RelationIdentity::new("public", "Mixed Name"),
        RelationIdentity::new("other", "mixed name"),
    ] {
        ensure_type_name_available(&catalog, &identity).unwrap();
    }
}
