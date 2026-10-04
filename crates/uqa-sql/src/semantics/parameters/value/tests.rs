//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{name_truncation_notice, parse_setting, show_setting};
use crate::semantics::parameters::catalog::find_parameter;
use crate::semantics::parameters::definition::{
    ParameterContext, ParameterDefinition, ParameterFlags, ParameterKind,
};
use crate::semantics::parameters::units::ParameterUnit;
use crate::SQLError;

fn defined(name: &str) -> &'static ParameterDefinition {
    find_parameter(name).unwrap_or_else(|| panic!("{name} is defined"))
}

fn timeout() -> ParameterDefinition {
    ParameterDefinition {
        name: "statement_timeout",
        kind: ParameterKind::Integer {
            boot: 0,
            min: 0,
            max: i32::MAX,
            unit: Some(ParameterUnit::Milliseconds),
        },
        context: ParameterContext::User,
        category: "Client Connection Defaults / Statement Behavior",
        short_desc: "Sets the maximum allowed duration of any statement.",
        extra_desc: Some("0 disables the timeout."),
        flags: ParameterFlags::NONE,
        library: None,
    }
}

fn diagnostic(error: SQLError) -> (String, String, Option<String>) {
    match error {
        SQLError::Diagnostic {
            sqlstate,
            message,
            hint,
            ..
        } => (sqlstate, message, hint),
        SQLError::Routine { sqlstate, message } => (sqlstate, message, None),
        other => panic!("unexpected error {other:?}"),
    }
}

#[test]
fn booleans_accept_the_spellings_of_parse_bool() {
    let definition = defined("enable_indexonlyscan");
    for (raw, setting) in [
        ("on", "on"),
        ("ON", "on"),
        ("of", "off"),
        ("off", "off"),
        ("t", "on"),
        ("TRUE", "on"),
        ("y", "on"),
        ("no", "off"),
        ("1", "on"),
        ("0", "off"),
    ] {
        assert_eq!(parse_setting(definition, raw).unwrap(), setting, "{raw:?}");
    }
    for raw in ["o", "maybe", "2", "10", "truex", " on", ""] {
        assert_eq!(
            diagnostic(parse_setting(definition, raw).unwrap_err()),
            (
                "22023".into(),
                "parameter \"enable_indexonlyscan\" requires a Boolean value".into(),
                None
            ),
            "{raw:?}"
        );
    }
}

#[test]
fn integers_report_postgres_range_errors_in_their_units() {
    let definition = timeout();
    assert_eq!(parse_setting(&definition, "1.5s").unwrap(), "1500");
    assert_eq!(
        diagnostic(parse_setting(&definition, "-1").unwrap_err()),
        (
            "22023".into(),
            "-1 ms is outside the valid range for parameter \"statement_timeout\" (0 ms .. 2147483647 ms)".into(),
            None
        )
    );
    assert_eq!(
        diagnostic(parse_setting(&definition, "1 xyz").unwrap_err()),
        (
            "22023".into(),
            "invalid value for parameter \"statement_timeout\": \"1 xyz\"".into(),
            Some("Valid units for this parameter are \"us\", \"ms\", \"s\", \"min\", \"h\", and \"d\".".into())
        )
    );
    let work_mem = defined("work_mem");
    assert_eq!(
        diagnostic(parse_setting(work_mem, "63").unwrap_err()).1,
        "63 kB is outside the valid range for parameter \"work_mem\" (64 kB .. 2147483647 kB)"
    );
    assert_eq!(
        show_setting(work_mem, &parse_setting(work_mem, "65536kB").unwrap()),
        "64MB"
    );
}

#[test]
fn enumerated_values_take_the_canonical_name_of_their_value() {
    let definition = defined("client_min_messages");
    for (raw, setting) in [
        ("WARNING", "warning"),
        ("info", "info"),
        ("debug", "debug2"),
        ("Debug1", "debug1"),
    ] {
        assert_eq!(parse_setting(definition, raw).unwrap(), setting);
    }
    assert_eq!(
        diagnostic(parse_setting(definition, "fatal").unwrap_err()),
        (
            "22023".into(),
            "invalid value for parameter \"client_min_messages\": \"fatal\"".into(),
            Some("Available values: debug5, debug4, debug3, debug2, debug1, log, notice, warning, error.".into())
        )
    );
    assert_eq!(
        parse_setting(defined("default_transaction_isolation"), "REPEATABLE READ").unwrap(),
        "repeatable read"
    );
}

#[test]
fn identifier_strings_are_truncated_with_a_notice() {
    let definition = defined("application_name");
    let long = "x".repeat(70);
    assert_eq!(parse_setting(definition, &long).unwrap(), "x".repeat(63));
    assert_eq!(
        name_truncation_notice(definition, &long),
        Some(format!(
            "identifier \"{long}\" will be truncated to \"{}\"",
            "x".repeat(63)
        ))
    );
    assert_eq!(name_truncation_notice(definition, &"x".repeat(63)), None);
    let multibyte = "\u{e9}".repeat(40);
    assert_eq!(
        parse_setting(definition, &multibyte).unwrap(),
        "\u{e9}".repeat(31)
    );
}

#[test]
fn definitions_report_postgres_names_and_former_names() {
    assert_eq!(defined("datestyle").name, "DateStyle");
    assert_eq!(defined("TIMEZONE").name, "TimeZone");
    assert_eq!(defined("sort_mem").name, "work_mem");
    assert!(find_parameter("nosuch").is_none());
    assert_eq!(defined("work_mem").boot_setting(), "4096");
    assert_eq!(defined("client_min_messages").boot_setting(), "notice");
    assert_eq!(
        defined("transaction_isolation").boot_setting(),
        "read committed"
    );
}
