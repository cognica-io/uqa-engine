//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One parameter's value in a session, as `pg_settings` and `SHOW ALL` report it.

use super::definition::ParameterDefinition;

/// A parameter's value in a session, with the value `RESET` restores and where the current value came from.
#[derive(Clone, Debug)]
pub struct ParameterSetting {
    pub definition: &'static ParameterDefinition,
    /// The setting in base units, as `pg_settings.setting` reports it.
    pub setting: String,
    /// The setting `RESET` restores, as `pg_settings.reset_val` reports it.
    pub reset_setting: String,
    /// Where the setting came from, as `pg_settings.source` reports it: `default`, `client`, `override` or `session`.
    pub source: &'static str,
}

impl ParameterSetting {
    /// The setting as `SHOW` reports it.
    pub fn shown(&self) -> String {
        super::value::show_setting(self.definition, &self.setting)
    }
}
