//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The configuration parameters the engine defines, with the types, bounds, contexts and descriptions of `PostgreSQL` 18's `guc_tables.c`, in the case-insensitive name order of `SHOW ALL`.

use super::definition::{
    EnumOption, ParameterContext, ParameterDefinition, ParameterFlags, ParameterKind,
};
use super::units::ParameterUnit;

const STATEMENT_BEHAVIOR: &str = "Client Connection Defaults / Statement Behavior";
const LOCALE_AND_FORMATTING: &str = "Client Connection Defaults / Locale and Formatting";
const PREVIOUS_VERSIONS: &str = "Version and Platform Compatibility / Previous PostgreSQL Versions";
const PRESET_OPTIONS: &str = "Preset Options";
const TIMEOUT_DISABLED: Option<&str> = Some("0 disables the timeout.");

/// The message levels of `PostgreSQL`'s `elog.h`, which order `client_min_messages`.
pub mod message_levels {
    pub const DEBUG5: u8 = 10;
    pub const DEBUG4: u8 = 11;
    pub const DEBUG3: u8 = 12;
    pub const DEBUG2: u8 = 13;
    pub const DEBUG1: u8 = 14;
    pub const LOG: u8 = 15;
    pub const INFO: u8 = 17;
    pub const NOTICE: u8 = 18;
    pub const WARNING: u8 = 19;
    pub const ERROR: u8 = 21;
}

const CLIENT_MESSAGE_LEVELS: &[EnumOption] = &[
    EnumOption::listed("debug5", message_levels::DEBUG5),
    EnumOption::listed("debug4", message_levels::DEBUG4),
    EnumOption::listed("debug3", message_levels::DEBUG3),
    EnumOption::listed("debug2", message_levels::DEBUG2),
    EnumOption::listed("debug1", message_levels::DEBUG1),
    EnumOption::hidden("debug", message_levels::DEBUG2),
    EnumOption::listed("log", message_levels::LOG),
    EnumOption::hidden("info", message_levels::INFO),
    EnumOption::listed("notice", message_levels::NOTICE),
    EnumOption::listed("warning", message_levels::WARNING),
    EnumOption::listed("error", message_levels::ERROR),
];

const ISOLATION_LEVELS: &[EnumOption] = &[
    EnumOption::listed("serializable", 3),
    EnumOption::listed("repeatable read", 2),
    EnumOption::listed("read committed", 1),
    EnumOption::listed("read uncommitted", 0),
];

const PLAN_CACHE_MODES: &[EnumOption] = &[
    EnumOption::listed("auto", 0),
    EnumOption::listed("force_generic_plan", 1),
    EnumOption::listed("force_custom_plan", 2),
];

/// The values of `plpgsql.variable_conflict`, as `plpgsql_variable_conflict` lists them.
const VARIABLE_CONFLICTS: &[EnumOption] = &[
    EnumOption::listed("error", 0),
    EnumOption::listed("use_variable", 1),
    EnumOption::listed("use_column", 2),
];
const REPLICATION_ROLES: &[EnumOption] = &[
    EnumOption::listed("origin", 0),
    EnumOption::listed("replica", 1),
    EnumOption::listed("local", 2),
];

const XML_OPTIONS: &[EnumOption] = &[
    EnumOption::listed("content", 1),
    EnumOption::listed("document", 0),
];

/// The version the engine reports as `server_version_num`.
pub const SERVER_VERSION_NUM: i32 = 180_000;

const fn define(
    name: &'static str,
    kind: ParameterKind,
    context: ParameterContext,
    category: &'static str,
    short_desc: &'static str,
    extra_desc: Option<&'static str>,
    flags: ParameterFlags,
) -> ParameterDefinition {
    ParameterDefinition {
        name,
        kind,
        context,
        category,
        short_desc,
        extra_desc,
        flags,
        library: None,
    }
}

const fn timeout(name: &'static str, short_desc: &'static str) -> ParameterDefinition {
    define(
        name,
        ParameterKind::Integer {
            boot: 0,
            min: 0,
            max: i32::MAX,
            unit: Some(ParameterUnit::Milliseconds),
        },
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        short_desc,
        TIMEOUT_DISABLED,
        ParameterFlags::NONE,
    )
}

const fn boolean(boot: bool) -> ParameterKind {
    ParameterKind::Bool { boot }
}

const fn string(boot: &'static str) -> ParameterKind {
    ParameterKind::String { boot }
}

const fn enumerated(boot: u8, options: &'static [EnumOption]) -> ParameterKind {
    ParameterKind::Enum { boot, options }
}

const NO_RESET: ParameterFlags = ParameterFlags::NO_RESET.union(ParameterFlags::NO_RESET_ALL);
const AUTHORIZATION: ParameterFlags = ParameterFlags::IS_NAME
    .union(ParameterFlags::NO_SHOW_ALL)
    .union(ParameterFlags::NO_RESET_ALL);

static PARAMETERS: &[ParameterDefinition] = &[
    define(
        "application_name",
        string(""),
        ParameterContext::User,
        "Reporting and Logging / What to Log",
        "Sets the application name to be reported in statistics and logs.",
        None,
        ParameterFlags::IS_NAME.union(ParameterFlags::REPORT),
    ),
    define(
        "check_function_bodies",
        boolean(true),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Check routine bodies during CREATE FUNCTION and CREATE PROCEDURE.",
        None,
        ParameterFlags::NONE,
    ),
    define(
        "client_encoding",
        string("SQL_ASCII"),
        ParameterContext::User,
        LOCALE_AND_FORMATTING,
        "Sets the client's character set encoding.",
        None,
        ParameterFlags::IS_NAME.union(ParameterFlags::REPORT),
    ),
    define(
        "client_min_messages",
        enumerated(message_levels::NOTICE, CLIENT_MESSAGE_LEVELS),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the message levels that are sent to the client.",
        Some("Each level includes all the levels that follow it. The later the level, the fewer messages are sent."),
        ParameterFlags::NONE,
    ),
    define(
        "DateStyle",
        string("ISO, MDY"),
        ParameterContext::User,
        LOCALE_AND_FORMATTING,
        "Sets the display format for date and time values.",
        Some("Also controls interpretation of ambiguous date inputs."),
        ParameterFlags::LIST_INPUT.union(ParameterFlags::REPORT),
    ),
    define(
        "default_table_access_method",
        string("heap"),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the default table access method for new tables.",
        None,
        ParameterFlags::IS_NAME,
    ),
    define(
        "default_tablespace",
        string(""),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the default tablespace to create tables and indexes in.",
        Some("An empty string means use the database's default tablespace."),
        ParameterFlags::IS_NAME,
    ),
    define(
        "default_transaction_deferrable",
        boolean(false),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the default deferrable status of new transactions.",
        None,
        ParameterFlags::NONE,
    ),
    define(
        "default_transaction_isolation",
        enumerated(1, ISOLATION_LEVELS),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the transaction isolation level of each new transaction.",
        None,
        ParameterFlags::NONE,
    ),
    define(
        "default_transaction_read_only",
        boolean(false),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the default read-only status of new transactions.",
        None,
        ParameterFlags::REPORT,
    ),
    define(
        "default_with_oids",
        boolean(false),
        ParameterContext::User,
        PREVIOUS_VERSIONS,
        "WITH OIDS is no longer supported; this can only be false.",
        None,
        ParameterFlags::NO_SHOW_ALL,
    ),
    define(
        "enable_indexonlyscan",
        boolean(true),
        ParameterContext::User,
        "Query Tuning / Planner Method Configuration",
        "Enables the planner's use of index-only-scan plans.",
        None,
        ParameterFlags::NONE,
    ),
    define(
        "escape_string_warning",
        boolean(true),
        ParameterContext::User,
        PREVIOUS_VERSIONS,
        "Warn about backslash escapes in ordinary string literals.",
        None,
        ParameterFlags::NONE,
    ),
    timeout(
        "idle_in_transaction_session_timeout",
        "Sets the maximum allowed idle time between queries, when in a transaction.",
    ),
    timeout(
        "idle_session_timeout",
        "Sets the maximum allowed idle time between queries, when not in a transaction.",
    ),
    define(
        "in_hot_standby",
        boolean(false),
        ParameterContext::Internal,
        PRESET_OPTIONS,
        "Shows whether hot standby is currently active.",
        None,
        ParameterFlags::REPORT,
    ),
    define(
        "integer_datetimes",
        boolean(true),
        ParameterContext::Internal,
        PRESET_OPTIONS,
        "Shows whether datetimes are integer based.",
        None,
        ParameterFlags::REPORT,
    ),
    define(
        "is_superuser",
        boolean(false),
        ParameterContext::Internal,
        PRESET_OPTIONS,
        "Shows whether the current user is a superuser.",
        None,
        ParameterFlags::REPORT
            .union(ParameterFlags::NO_SHOW_ALL)
            .union(ParameterFlags::NO_RESET_ALL),
    ),
    timeout(
        "lock_timeout",
        "Sets the maximum allowed duration of any wait for a lock.",
    ),
    define(
        "plan_cache_mode",
        enumerated(0, PLAN_CACHE_MODES),
        ParameterContext::User,
        "Query Tuning / Other Planner Options",
        "Controls the planner's selection of custom or generic plan.",
        Some("Prepared statements can have custom and generic plans, and the planner will attempt to choose which is better.  This can be set to override the default behavior."),
        ParameterFlags::NONE,
    ),
    ParameterDefinition {
        library: Some("plpgsql"),
        ..define(
            "plpgsql.check_asserts",
            boolean(true),
            ParameterContext::User,
            "Customized Options",
            "Perform checks given in ASSERT statements.",
            None,
            ParameterFlags::NONE,
        )
    },
    ParameterDefinition {
        library: Some("plpgsql"),
        ..define(
            "plpgsql.variable_conflict",
            enumerated(0, VARIABLE_CONFLICTS),
            ParameterContext::Superuser,
            "Customized Options",
            "Sets handling of conflicts between PL/pgSQL variable names and table column names.",
            None,
            ParameterFlags::NONE,
        )
    },
    define(
        "role",
        string("none"),
        ParameterContext::User,
        "Ungrouped",
        "Sets the current role.",
        None,
        AUTHORIZATION,
    ),
    define(
        "row_security",
        boolean(true),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Enables row security.",
        Some("When enabled, row security will be applied to all users."),
        ParameterFlags::NONE,
    ),
    define(
        "search_path",
        string("\"$user\", public"),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the schema search order for names that are not schema-qualified.",
        None,
        ParameterFlags::LIST_INPUT
            .union(ParameterFlags::LIST_QUOTE)
            .union(ParameterFlags::REPORT),
    ),
    define(
        "server_encoding",
        string("SQL_ASCII"),
        ParameterContext::Internal,
        PRESET_OPTIONS,
        "Shows the server (database) character set encoding.",
        None,
        ParameterFlags::IS_NAME.union(ParameterFlags::REPORT),
    ),
    define(
        "server_version",
        string("18.0-uqa"),
        ParameterContext::Internal,
        PRESET_OPTIONS,
        "Shows the server version.",
        None,
        ParameterFlags::REPORT,
    ),
    ParameterDefinition {
        name: "server_version_num",
        kind: ParameterKind::Integer {
            boot: SERVER_VERSION_NUM,
            min: SERVER_VERSION_NUM,
            max: SERVER_VERSION_NUM,
            unit: None,
        },
        context: ParameterContext::Internal,
        category: PRESET_OPTIONS,
        short_desc: "Shows the server version as an integer.",
        extra_desc: None,
        flags: ParameterFlags::NONE,
        library: None,
    },
    define(
        "session_authorization",
        string(""),
        ParameterContext::User,
        "Ungrouped",
        "Sets the session user name.",
        None,
        AUTHORIZATION.union(ParameterFlags::REPORT),
    ),
    define(
        "session_replication_role",
        enumerated(0, REPLICATION_ROLES),
        ParameterContext::Superuser,
        STATEMENT_BEHAVIOR,
        "Sets the session's behavior for triggers and rewrite rules.",
        None,
        ParameterFlags::NONE,
    ),
    define(
        "standard_conforming_strings",
        boolean(true),
        ParameterContext::User,
        PREVIOUS_VERSIONS,
        "Causes '...' strings to treat backslashes literally.",
        None,
        ParameterFlags::REPORT,
    ),
    timeout(
        "statement_timeout",
        "Sets the maximum allowed duration of any statement.",
    ),
    define(
        "TimeZone",
        string("GMT"),
        ParameterContext::User,
        LOCALE_AND_FORMATTING,
        "Sets the time zone for displaying and interpreting time stamps.",
        None,
        ParameterFlags::REPORT,
    ),
    define(
        "transaction_deferrable",
        boolean(false),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Whether to defer a read-only serializable transaction until it can be executed with no possible serialization failures.",
        None,
        NO_RESET,
    ),
    define(
        "transaction_isolation",
        enumerated(1, ISOLATION_LEVELS),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the current transaction's isolation level.",
        None,
        NO_RESET,
    ),
    define(
        "transaction_read_only",
        boolean(false),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets the current transaction's read-only status.",
        None,
        NO_RESET,
    ),
    timeout(
        "transaction_timeout",
        "Sets the maximum allowed duration of any transaction within a session (not a prepared transaction).",
    ),
    ParameterDefinition {
        name: "work_mem",
        kind: ParameterKind::Integer {
            boot: 4096,
            min: 64,
            max: i32::MAX,
            unit: Some(ParameterUnit::Kilobytes),
        },
        context: ParameterContext::User,
        category: "Resource Usage / Memory",
        short_desc: "Sets the maximum memory to be used for query workspaces.",
        extra_desc: Some("This much memory can be used by each internal sort operation and hash table before switching to temporary disk files."),
        flags: ParameterFlags::NONE,
        library: None,
    },
    define(
        "xmloption",
        enumerated(1, XML_OPTIONS),
        ParameterContext::User,
        STATEMENT_BEHAVIOR,
        "Sets whether XML data in implicit parsing and serialization operations is to be considered as documents or content fragments.",
        None,
        ParameterFlags::NONE,
    ),
];

/// The names that `PostgreSQL` still accepts for renamed parameters (`map_old_guc_names`).
const OLD_NAMES: &[(&str, &str)] = &[("sort_mem", "work_mem")];

/// Every parameter the engine defines, in `SHOW ALL` order.
pub fn parameter_definitions() -> &'static [ParameterDefinition] {
    PARAMETERS
}

/// The parameter `name` refers to, in any case and under a former name.
pub fn find_parameter(name: &str) -> Option<&'static ParameterDefinition> {
    let name = OLD_NAMES
        .iter()
        .find(|(old, _)| old.eq_ignore_ascii_case(name))
        .map_or(name, |(_, current)| current);
    PARAMETERS
        .iter()
        .find(|definition| definition.name.eq_ignore_ascii_case(name))
}
