//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Apache AGE graph and label name validation.
//!
//! AGE validates names with anchored regular expressions built from the
//! Unicode `ID_Start` and `ID_Continue` classes (`name_validation.c`):
//!
//! - graph names: `^[ID_Start_][ID_Continue.-]*[ID_Continue]$`, 3 to 63 bytes
//! - label names: `^[ID_Start_][ID_Continue]*$`, 1 to 63 bytes
//!
//! Lengths are byte lengths because AGE measures them with `strlen`.

use icu_properties::props::{IdContinue, IdStart};
use icu_properties::CodePointSetData;

/// AGE `MIN_GRAPH_NAME_LEN`.
pub const MIN_GRAPH_NAME_LEN: usize = 3;
/// AGE `MAX_GRAPH_NAME_LEN`.
pub const MAX_GRAPH_NAME_LEN: usize = 63;
/// AGE `MIN_LABEL_NAME_LEN`.
pub const MIN_LABEL_NAME_LEN: usize = 1;
/// AGE `MAX_LABEL_NAME_LEN` (`NAMEDATALEN - 1`).
pub const MAX_LABEL_NAME_LEN: usize = 63;

/// Name of the AGE default vertex label that every graph owns.
pub const VERTEX_DEFAULT_LABEL_NAME: &str = "_ag_label_vertex";
/// Name of the AGE default edge label that every graph owns.
pub const EDGE_DEFAULT_LABEL_NAME: &str = "_ag_label_edge";

fn is_id_start(c: char) -> bool {
    c == '_' || CodePointSetData::new::<IdStart>().contains(c)
}

fn is_id_continue(c: char) -> bool {
    CodePointSetData::new::<IdContinue>().contains(c)
}

/// AGE `is_valid_graph_name`: 3 to 63 bytes, an `ID_Start` character or
/// underscore first, `ID_Continue` characters plus `.` and `-` in the
/// middle, and an `ID_Continue` character last.
#[must_use]
pub fn is_valid_graph_name(name: &str) -> bool {
    if name.len() < MIN_GRAPH_NAME_LEN || name.len() > MAX_GRAPH_NAME_LEN {
        return false;
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !is_id_start(first) {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    // The anchored pattern needs a distinct last character, so a single
    // multi-byte character that clears the byte minimum is still invalid.
    let Some((last, middle)) = rest.split_last() else {
        return false;
    };
    middle
        .iter()
        .all(|c| is_id_continue(*c) || *c == '.' || *c == '-')
        && is_id_continue(*last)
}

/// AGE `is_valid_label_name`: 1 to 63 bytes, an `ID_Start` character or
/// underscore first, and `ID_Continue` characters after it.
#[must_use]
pub fn is_valid_label_name(name: &str) -> bool {
    if name.len() < MIN_LABEL_NAME_LEN || name.len() > MAX_LABEL_NAME_LEN {
        return false;
    }
    let mut chars = name.chars();
    chars.next().is_some_and(is_id_start) && chars.all(is_id_continue)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_names_follow_age_rules() {
        assert!(is_valid_graph_name("demo"));
        assert!(is_valid_graph_name("_g1"));
        assert!(is_valid_graph_name("g_c1"));
        assert!(is_valid_graph_name("my.graph-2"));
        assert!(is_valid_graph_name("한글그래프"));
        assert!(!is_valid_graph_name("g1"), "shorter than three bytes");
        assert!(!is_valid_graph_name("1abc"), "must not start with a digit");
        assert!(!is_valid_graph_name("abc."), "must not end with a dot");
        assert!(!is_valid_graph_name("abc-"), "must not end with a dash");
        assert!(!is_valid_graph_name("a b"), "no spaces");
        assert!(
            !is_valid_graph_name(&"x".repeat(64)),
            "longer than 63 bytes"
        );
        assert!(is_valid_graph_name(&"x".repeat(63)));
    }

    #[test]
    fn label_names_follow_age_rules() {
        assert!(is_valid_label_name("Person"));
        assert!(is_valid_label_name("_"));
        assert!(is_valid_label_name("v1"));
        assert!(is_valid_label_name("KNOWS"));
        assert!(is_valid_label_name(VERTEX_DEFAULT_LABEL_NAME));
        assert!(!is_valid_label_name(""));
        assert!(!is_valid_label_name("1v"));
        assert!(!is_valid_label_name("has-dash"));
        assert!(!is_valid_label_name("has.dot"));
        assert!(!is_valid_label_name(&"x".repeat(64)));
    }
}
