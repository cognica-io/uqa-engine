//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

/// `REG_ADVANCED` with the bits of `added` set and those of `removed` cleared, as `parse_re_flags` computes them.
fn advanced(added: CompileFlags, removed: CompileFlags) -> CompileFlags {
    (REG_ADVANCED | added) & !removed
}

#[test]
fn flags_select_postgresql_compile_options_in_order() {
    let none = CompileFlags::default();
    for (flags, cflags, global) in [
        ("", REG_ADVANCED, false),
        ("g", REG_ADVANCED, true),
        ("i", advanced(REG_ICASE, none), false),
        ("ic", REG_ADVANCED, false),
        ("ci", advanced(REG_ICASE, none), false),
        ("b", none, false),
        // `e` sets `REG_EXTENDED` and then clears `REG_ADVANCED`, which contains it.
        ("e", none, false),
        ("q", REG_QUOTE, false),
        ("qb", none, false),
        ("bq", REG_QUOTE, false),
        ("n", advanced(REG_NEWLINE, none), false),
        ("m", advanced(REG_NEWLINE, none), false),
        ("ns", REG_ADVANCED, false),
        ("p", advanced(REG_NLSTOP, none), false),
        ("np", advanced(REG_NLSTOP, none), false),
        ("w", advanced(REG_NLANCH, none), false),
        ("nw", advanced(REG_NLANCH, none), false),
        ("x", advanced(REG_EXPANDED, none), false),
        ("xt", REG_ADVANCED, false),
        ("gix", advanced(REG_ICASE | REG_EXPANDED, none), true),
    ] {
        assert_eq!(
            ReFlags::parse(flags).unwrap(),
            ReFlags { cflags, global },
            "{flags:?}"
        );
    }
}

#[test]
fn an_unknown_flag_reports_its_whole_character() {
    for (flags, shown) in [("z", "z"), ("iz", "z"), ("i\u{e9}x", "\u{e9}"), ("G", "G")] {
        let error = ReFlags::parse(flags).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22023"), "{flags:?}");
        assert_eq!(
            error.to_string(),
            format!("invalid regular expression option: \"{shown}\""),
            "{flags:?}"
        );
    }
}

#[test]
fn single_result_functions_reject_global_with_their_hint() {
    let global = ReFlags::parse("ig").unwrap();
    let error = global
        .reject_global(SingleResult::named("regexp_count"))
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("22023"));
    assert_eq!(
        error.to_string(),
        "regexp_count() does not support the \"global\" option"
    );
    let SQLError::Diagnostic { hint, .. } = global
        .reject_global(SingleResult {
            function: "regexp_match",
            hint: Some("Use the regexp_matches function instead."),
        })
        .unwrap_err()
    else {
        panic!("diagnostic with a hint")
    };
    assert_eq!(
        hint.as_deref(),
        Some("Use the regexp_matches function instead.")
    );
    let single = ReFlags::parse("i").unwrap();
    assert_eq!(
        single
            .reject_global(SingleResult::named("regexp_count"))
            .unwrap()
            .every_match(),
        ReFlags {
            cflags: single.cflags,
            global: true
        }
    );
}
