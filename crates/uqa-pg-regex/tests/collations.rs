//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Collation selection with `pg_set_regex_collation`'s order of checks.

use uqa_pg_regex::{CollationCtype, CollationLocale, RegexCollation, RegexError};

#[test]
fn predefined_collations_select_their_postgresql_strategies() {
    for (oid, expected) in [
        (100, RegexCollation::BuiltinUnicodeFast),
        (811, RegexCollation::BuiltinCUtf8),
        (950, RegexCollation::C),
        (951, RegexCollation::C),
        (962, RegexCollation::C),
        (963, RegexCollation::Icu),
        (6411, RegexCollation::BuiltinUnicodeFast),
    ] {
        assert_eq!(RegexCollation::from_oid(oid), Ok(expected), "OID {oid}");
    }
}

#[test]
fn an_unresolved_collation_fails_before_any_other_check() {
    let nondeterministic = CollationLocale {
        deterministic: false,
        ctype: CollationCtype::Icu,
    };
    for error in [
        RegexCollation::from_oid(0).unwrap_err(),
        RegexCollation::from_locale(0, nondeterministic).unwrap_err(),
    ] {
        assert_eq!(error, RegexError::IndeterminateCollation);
        assert_eq!(error.sqlstate(), Some("42P22"));
        assert_eq!(
            error.message(),
            "could not determine which collation to use for regular expression"
        );
        assert_eq!(
            error.hint(),
            Some("Use the COLLATE clause to set the collation explicitly.")
        );
    }
}

#[test]
fn the_c_collation_is_accepted_before_its_properties_are_consulted() {
    let nondeterministic = CollationLocale {
        deterministic: false,
        ctype: CollationCtype::Icu,
    };
    assert_eq!(
        RegexCollation::from_locale(950, nondeterministic),
        Ok(RegexCollation::C)
    );
}

#[test]
fn an_unknown_collation_fails_as_a_catalog_lookup() {
    let error = RegexCollation::from_oid(4242).unwrap_err();
    assert_eq!(error, RegexError::UnknownCollation(4242));
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert_eq!(error.hint(), None);
    assert_eq!(error.message(), "cache lookup failed for collation 4242");
}

#[test]
fn collation_properties_select_strategies() {
    for (ctype, expected) in [
        (CollationCtype::C, RegexCollation::C),
        (
            CollationCtype::Builtin {
                casemap_full: false,
            },
            RegexCollation::BuiltinCUtf8,
        ),
        (
            CollationCtype::Builtin { casemap_full: true },
            RegexCollation::BuiltinUnicodeFast,
        ),
        (CollationCtype::Icu, RegexCollation::Icu),
    ] {
        let locale = CollationLocale {
            deterministic: true,
            ctype,
        };
        assert_eq!(RegexCollation::from_locale(16384, locale), Ok(expected));
    }
}

#[test]
fn icu_classes_reject_nul_surrogates_and_out_of_range_codes() {
    use uqa_pg_regex::{MatchSpan, Regex, REG_ADVANCED};

    // PostgreSQL text cannot carry these inputs. Regex::exec accepts code points directly.
    let points: Vec<_> = [0]
        .into_iter()
        .chain(0xD800..=0xDFFF)
        .chain([0x11_0000, 0x7FFF_FFFE])
        .collect();
    for class in [
        "digit", "alpha", "alnum", "upper", "lower", "graph", "print", "punct", "space",
    ] {
        let mut budget = crate::support::Budget::unlimited();
        let mut regex = Regex::compile(
            &format!("[[:{class}:]]"),
            REG_ADVANCED,
            RegexCollation::Icu,
            &mut budget,
        )
        .unwrap();
        let mut span = [MatchSpan::UNSET];
        assert!(
            !regex.exec(&points, 0, &mut span).unwrap(),
            "{class} unexpectedly matched {:?}",
            span[0].range()
        );
    }
}
