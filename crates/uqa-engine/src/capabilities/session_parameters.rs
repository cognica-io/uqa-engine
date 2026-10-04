//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The configuration parameters a session sees: what a name refers to, the setting it holds, and the values `SHOW`, `current_setting`, `SHOW ALL` and `pg_settings` report.

use uqa_sql::catalog::roles::{memberships::role_is_superuser, RoleReference};
use uqa_sql::semantics::parameters::catalog::{find_parameter, parameter_definitions};
use uqa_sql::semantics::parameters::custom::unrecognized_parameter;
use uqa_sql::semantics::parameters::definition::{ParameterDefinition, ParameterFlags};
use uqa_sql::semantics::parameters::setting::ParameterSetting;
use uqa_sql::semantics::parameters::value::show_setting;
use uqa_sql::SQLError;

use super::{SessionContext, SessionExecutionView};

/// What a parameter name refers to in a session.
pub(crate) enum SessionParameter {
    /// A parameter the engine defines, or one that a library the session has loaded defines.
    Defined(&'static ParameterDefinition),
    /// A custom parameter the session has created, by the name it was created with.
    Placeholder(String),
}

/// The setting the engine starts a session with where it differs from `PostgreSQL`'s boot value: the encodings of the database, the server's time zone, and the engine's working memory.
pub(crate) fn engine_default(definition: &ParameterDefinition) -> Option<&'static str> {
    match definition.name {
        "client_encoding" | "server_encoding" => Some("UTF8"),
        "TimeZone" => Some("UTC"),
        "work_mem" => Some("65536"),
        _ => None,
    }
}

/// The key under which a session keeps the setting of `name`.
pub(crate) fn setting_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

/// The key under which a session keeps the setting of the parameter `name` refers to: a defined parameter's canonical name under any spelling, or the custom name.
pub(crate) fn parameter_key(name: &str) -> String {
    find_parameter(name).map_or_else(
        || setting_key(name),
        |definition| setting_key(definition.name),
    )
}

impl SessionContext {
    /// The setting of the ordinary parameter `name` in base units: the session's own, the client's startup setting, the engine's default or the boot value.
    pub(crate) fn setting(&self, name: &str) -> String {
        let definition = find_parameter(name).expect("the engine defines the parameter");
        if let Some(setting) = self
            .state
            .read()
            .session_vars
            .get(&setting_key(definition.name))
        {
            return setting.clone();
        }
        self.reset_setting(definition)
    }

    /// The setting `RESET` restores: the client's startup setting, the engine's default or the boot value.
    pub(crate) fn reset_setting(&self, definition: &ParameterDefinition) -> String {
        if let Some(setting) = self.parameters.lock().client_setting(definition.name) {
            return setting.to_string();
        }
        engine_default(definition).map_or_else(|| definition.boot_setting(), str::to_string)
    }
}

impl SessionExecutionView<'_> {
    /// What `name` refers to: a defined parameter, or an existing placeholder. A parameter of a library the session has not loaded is a placeholder until the library loads.
    pub(crate) fn resolve_parameter(&self, name: &str) -> Option<SessionParameter> {
        let registry = self.session.parameters.lock();
        if let Some(definition) = find_parameter(name) {
            if definition
                .library
                .is_none_or(|library| registry.library_loaded(library))
            {
                return Some(SessionParameter::Defined(definition));
            }
        }
        registry
            .placeholder(name)
            .map(|name| SessionParameter::Placeholder(name.to_string()))
    }

    /// The setting `RESET` restores: the client's startup setting, the engine's default or the boot value.
    pub(crate) fn reset_setting(&self, definition: &ParameterDefinition) -> String {
        self.session.reset_setting(definition)
    }

    /// The setting `definition` holds in base units, with where it came from.
    pub(crate) fn parameter_setting(
        &self,
        definition: &ParameterDefinition,
    ) -> (String, &'static str) {
        if let Some((setting, assigned)) = self.transaction_parameter_setting(definition.name) {
            return (setting, if assigned { "session" } else { "override" });
        }
        let state = self.session.state.read();
        match definition.name {
            "role" => return (state.authorization.show_role().to_owned(), "default"),
            "session_authorization" => {
                return (state.authorization.session().name.clone(), "default");
            }
            "is_superuser" => {
                let outer = RoleReference::Bound(state.authorization.outer().clone());
                let superuser = role_is_superuser(&self.durable.roles.read(), &outer);
                return (if superuser { "on" } else { "off" }.into(), "default");
            }
            _ => {}
        }
        if let Some(setting) = state.session_vars.get(&setting_key(definition.name)) {
            return (setting.clone(), "session");
        }
        drop(state);
        if let Some(setting) = self
            .session
            .parameters
            .lock()
            .client_setting(definition.name)
        {
            return (setting.to_string(), "client");
        }
        (self.reset_setting(definition), "default")
    }

    /// The setting of a placeholder, empty until the session assigns one.
    pub(crate) fn placeholder_setting(&self, name: &str) -> String {
        self.session
            .state
            .read()
            .session_vars
            .get(&setting_key(name))
            .cloned()
            .unwrap_or_default()
    }

    /// The canonical name of the parameter `name` refers to and its value as `SHOW` reports it.
    pub(crate) fn show_parameter(&self, name: &str) -> Result<(String, String), SQLError> {
        match self.resolve_parameter(name) {
            Some(SessionParameter::Defined(definition)) => {
                let (setting, _) = self.parameter_setting(definition);
                Ok((
                    definition.name.to_string(),
                    show_setting(definition, &setting),
                ))
            }
            Some(SessionParameter::Placeholder(name)) => {
                let setting = self.placeholder_setting(&name);
                Ok((name, setting))
            }
            None => Err(unrecognized_parameter(name)),
        }
    }

    pub(crate) fn show_variable(&self, name: &str) -> Result<String, SQLError> {
        self.show_parameter(name).map(|(_, value)| value)
    }

    /// The value `current_setting(name)` returns, or `None` for a name nothing defines.
    pub(crate) fn runtime_parameter(&self, name: &str) -> Option<String> {
        self.show_parameter(name).ok().map(|(_, value)| value)
    }

    /// Every parameter that `SHOW ALL` and `pg_settings` report, in name order: the defined parameters other than hidden ones, with those of the libraries the session has loaded.
    pub(crate) fn parameter_settings(&self) -> Vec<ParameterSetting> {
        parameter_definitions()
            .iter()
            .filter(|definition| !definition.has_flag(ParameterFlags::NO_SHOW_ALL))
            .filter(|definition| {
                matches!(
                    self.resolve_parameter(definition.name),
                    Some(SessionParameter::Defined(_))
                )
            })
            .map(|definition| {
                let (setting, source) = self.parameter_setting(definition);
                ParameterSetting {
                    definition,
                    setting,
                    reset_setting: self.reset_setting(definition),
                    source,
                }
            })
            .collect()
    }
}
