//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The definition of one configuration parameter as `PostgreSQL`'s `guc_tables.c` declares it: its type and bounds, when a session may change it, and the descriptions that `pg_settings` and `SHOW ALL` report.

use super::units::ParameterUnit;

/// When a session may change a parameter (`GucContext`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterContext {
    /// Fixed for the server: `SET` reports `parameter "..." cannot be changed`.
    Internal,
    /// Changed only by superusers and roles granted `SET` on the parameter.
    Superuser,
    /// Changed by any user.
    User,
}

impl ParameterContext {
    /// The name that `pg_settings.context` reports.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Superuser => "superuser",
            Self::User => "user",
        }
    }
}

/// One accepted value of an enumerated parameter (`config_enum_entry`). Values that share `value` are spellings of one setting, which displays as the first of them.
#[derive(Clone, Copy, Debug)]
pub struct EnumOption {
    pub name: &'static str,
    pub value: u8,
    /// Accepted but left out of `pg_settings.enumvals` and of the hint that lists the available values.
    pub hidden: bool,
}

impl EnumOption {
    pub const fn listed(name: &'static str, value: u8) -> Self {
        Self {
            name,
            value,
            hidden: false,
        }
    }

    pub const fn hidden(name: &'static str, value: u8) -> Self {
        Self {
            name,
            value,
            hidden: true,
        }
    }
}

/// The type of a parameter's value with its boot value and bounds.
#[derive(Clone, Copy, Debug)]
pub enum ParameterKind {
    Bool {
        boot: bool,
    },
    Integer {
        boot: i32,
        min: i32,
        max: i32,
        unit: Option<ParameterUnit>,
    },
    Enum {
        boot: u8,
        options: &'static [EnumOption],
    },
    String {
        boot: &'static str,
    },
}

impl ParameterKind {
    /// The name that `pg_settings.vartype` reports.
    pub const fn type_name(&self) -> &'static str {
        match self {
            Self::Bool { .. } => "bool",
            Self::Integer { .. } => "integer",
            Self::Enum { .. } => "enum",
            Self::String { .. } => "string",
        }
    }
}

/// The `GUC_*` flags of a parameter that change how SQL treats it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ParameterFlags(u16);

impl ParameterFlags {
    pub const NONE: Self = Self(0);
    /// `SET` accepts a list of values, which it joins with `, ` (`GUC_LIST_INPUT`).
    pub const LIST_INPUT: Self = Self(1 << 0);
    /// `SET` quotes each string of the list as an identifier needs (`GUC_LIST_QUOTE`).
    pub const LIST_QUOTE: Self = Self(1 << 1);
    /// Left out of `SHOW ALL` and `pg_settings` (`GUC_NO_SHOW_ALL`).
    pub const NO_SHOW_ALL: Self = Self(1 << 2);
    /// `RESET` and `SET ... TO DEFAULT` do not apply to it (`GUC_NO_RESET`).
    pub const NO_RESET: Self = Self(1 << 3);
    /// `RESET ALL` leaves it alone (`GUC_NO_RESET_ALL`).
    pub const NO_RESET_ALL: Self = Self(1 << 4);
    /// The server reports every change to the client in a `ParameterStatus` message (`GUC_REPORT`).
    pub const REPORT: Self = Self(1 << 5);
    /// A string value is truncated to the length of an identifier (`GUC_IS_NAME`).
    pub const IS_NAME: Self = Self(1 << 6);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// One configuration parameter.
#[derive(Clone, Copy, Debug)]
pub struct ParameterDefinition {
    /// The canonical spelling, which `SHOW` reports as its column name.
    pub name: &'static str,
    pub kind: ParameterKind,
    pub context: ParameterContext,
    pub category: &'static str,
    pub short_desc: &'static str,
    pub extra_desc: Option<&'static str>,
    pub flags: ParameterFlags,
    /// The library that defines the parameter when a session loads it; until then the name is an ordinary custom parameter of the library's reserved prefix.
    pub library: Option<&'static str>,
}

impl ParameterDefinition {
    pub fn has_flag(&self, flag: ParameterFlags) -> bool {
        self.flags.contains(flag)
    }

    /// The setting a session starts with, as `pg_settings.boot_val` reports it.
    pub fn boot_setting(&self) -> String {
        match self.kind {
            ParameterKind::Bool { boot } => if boot { "on" } else { "off" }.into(),
            ParameterKind::Integer { boot, .. } => boot.to_string(),
            ParameterKind::Enum { boot, options } => {
                super::value::enum_display(options, boot).into()
            }
            ParameterKind::String { boot } => boot.into(),
        }
    }

    /// The unit of an integer parameter, as `pg_settings.unit` reports it.
    pub fn unit(&self) -> Option<ParameterUnit> {
        match self.kind {
            ParameterKind::Integer { unit, .. } => unit,
            _ => None,
        }
    }
}
