//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18's regular-expression function signatures. Every `pg_proc` overload of `regexp_count`, `regexp_instr`, `regexp_like`, `regexp_match`, `regexp_matches`, `regexp_replace`, `regexp_split_to_array`, `regexp_split_to_table` and `regexp_substr` is a separate declaration with its argument names and no defaults, so overload resolution, named arguments and untyped parameters select exactly the catalog routine `PostgreSQL` selects.

use super::registry::{declarations, Signature};
use crate::ast::ColumnType as T;
use std::sync::LazyLock;

declarations! { fn lookup_fixed(local);
        "regexp_count" => &[
            Signature::named(&[T::Text, T::Text], &["string", "pattern"], T::Integer),
            Signature::named(&[T::Text, T::Text, T::Integer], &["string", "pattern", "start"], T::Integer),
            Signature::named(
                &[T::Text, T::Text, T::Integer, T::Text],
                &["string", "pattern", "start", "flags"],
                T::Integer,
            ),
        ],
        "regexp_instr" => &[
            Signature::named(&[T::Text, T::Text], &["string", "pattern"], T::Integer),
            Signature::named(&[T::Text, T::Text, T::Integer], &["string", "pattern", "start"], T::Integer),
            Signature::named(
                &[T::Text, T::Text, T::Integer, T::Integer],
                &["string", "pattern", "start", "N"],
                T::Integer,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Integer, T::Integer, T::Integer],
                &["string", "pattern", "start", "N", "endoption"],
                T::Integer,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Integer, T::Integer, T::Integer, T::Text],
                &["string", "pattern", "start", "N", "endoption", "flags"],
                T::Integer,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Integer, T::Integer, T::Integer, T::Text, T::Integer],
                &["string", "pattern", "start", "N", "endoption", "flags", "subexpr"],
                T::Integer,
            ),
        ],
        "regexp_like" => &[
            Signature::named(&[T::Text, T::Text], &["string", "pattern"], T::Boolean),
            Signature::named(&[T::Text, T::Text, T::Text], &["string", "pattern", "flags"], T::Boolean),
        ],
        "regexp_replace" => &[
            Signature::named(
                &[T::Text, T::Text, T::Text],
                &["string", "pattern", "replacement"],
                T::Text,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Text, T::Text],
                &["string", "pattern", "replacement", "flags"],
                T::Text,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Text, T::Integer],
                &["string", "pattern", "replacement", "start"],
                T::Text,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Text, T::Integer, T::Integer],
                &["string", "pattern", "replacement", "start", "N"],
                T::Text,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Text, T::Integer, T::Integer, T::Text],
                &["string", "pattern", "replacement", "start", "N", "flags"],
                T::Text,
            ),
        ],
        "regexp_split_to_table" => &[
            Signature::named(&[T::Text, T::Text], &["string", "pattern"], T::Text),
            Signature::named(&[T::Text, T::Text, T::Text], &["string", "pattern", "flags"], T::Text),
        ],
        "regexp_substr" => &[
            Signature::named(&[T::Text, T::Text], &["string", "pattern"], T::Text),
            Signature::named(&[T::Text, T::Text, T::Integer], &["string", "pattern", "start"], T::Text),
            Signature::named(
                &[T::Text, T::Text, T::Integer, T::Integer],
                &["string", "pattern", "start", "N"],
                T::Text,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Integer, T::Integer, T::Text],
                &["string", "pattern", "start", "N", "flags"],
                T::Text,
            ),
            Signature::named(
                &[T::Text, T::Text, T::Integer, T::Integer, T::Text, T::Integer],
                &["string", "pattern", "start", "N", "flags", "subexpr"],
                T::Text,
            ),
        ],
}

/// The functions whose two overloads return `text[]`, a result type without a constant constructor.
const TEXT_ARRAY_FUNCTIONS: [&str; 3] = ["regexp_match", "regexp_matches", "regexp_split_to_array"];

static TEXT_ARRAY_SIGNATURES: LazyLock<[Signature; 2]> = LazyLock::new(|| {
    let text_array = || T::Array(Box::new(T::Text));
    [
        Signature::named(&[T::Text, T::Text], &["string", "pattern"], text_array()),
        Signature::named(
            &[T::Text, T::Text, T::Text],
            &["string", "pattern", "flags"],
            text_array(),
        ),
    ]
});

pub(super) fn lookup(local: &str) -> Option<(&'static str, &'static [Signature])> {
    if let Some(declarations) = lookup_fixed(local) {
        return Some(declarations);
    }
    TEXT_ARRAY_FUNCTIONS
        .into_iter()
        .find(|name| local.eq_ignore_ascii_case(name))
        .map(|name| (name, &TEXT_ARRAY_SIGNATURES[..]))
}
