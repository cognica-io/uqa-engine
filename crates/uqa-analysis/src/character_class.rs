//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immutable Unicode ranges shared by native classification and case conversion.

use regex_syntax::hir::{Class, ClassUnicode, HirKind};

pub(crate) fn class(pattern: &str) -> Result<ClassUnicode, String> {
    let expression = regex_syntax::Parser::new()
        .parse(pattern)
        .map_err(|error| error.to_string())?;
    match expression.into_kind() {
        HirKind::Class(Class::Unicode(class)) => Ok(class),
        _ => Err("character property is not a Unicode class".into()),
    }
}

pub(crate) fn contains(class: &ClassUnicode, character: char) -> bool {
    class
        .ranges()
        .binary_search_by(|range| {
            if range.end() < character {
                std::cmp::Ordering::Less
            } else if range.start() > character {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}
