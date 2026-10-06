//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Assigning configuration parameters: `SET`, `RESET`, `RESET ALL`, `set_config` and the settings a client gives at startup, with `PostgreSQL`'s contexts, value checks and custom placeholders, and the libraries that define their parameters as a session loads them.

use uqa_sql::catalog::access_methods::{access_method_kind, AccessMethodKind};
use uqa_sql::semantics::parameters::custom::{
    check_assignable_custom_name, reserved_placeholder_warning,
};
use uqa_sql::semantics::parameters::definition::{
    ParameterContext, ParameterDefinition, ParameterFlags,
};
use uqa_sql::semantics::parameters::identifier_list::split_identifier_list;
use uqa_sql::semantics::parameters::value::{
    invalid_value_message, name_truncation_notice, parse_setting,
};
use uqa_sql::SQLNotice;

use super::{default_search_path, Engine, SQLError};
use crate::capabilities::session_parameters::{setting_key, SessionParameter};

/// The tablespaces of the database: its default one and the one for shared relations.
const TABLESPACES: [&str; 2] = ["pg_default", "pg_global"];

fn invalid_value(definition: &ParameterDefinition, value: &str, detail: String) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "22023".into(),
        message: invalid_value_message(definition, value),
        detail: Some(detail),
        hint: None,
    }
}

/// Whether `definition` is a characteristic of the current transaction, which the transaction rather than the session keeps.
fn is_transaction_characteristic(definition: &ParameterDefinition) -> bool {
    matches!(
        definition.name,
        "transaction_isolation" | "transaction_read_only" | "transaction_deferrable"
    )
}

/// `pg_clean_ascii`: every byte outside printable ASCII written as `\xNN`, as `application_name` keeps it.
fn clean_ascii(value: &str) -> String {
    use std::fmt::Write;
    let mut cleaned = String::with_capacity(value.len());
    for byte in value.bytes() {
        if (32..127).contains(&byte) {
            cleaned.push(char::from(byte));
        } else {
            write!(cleaned, "\\x{byte:02x}").expect("writing into a String cannot fail");
        }
    }
    cleaned
}

impl Engine {
    pub(crate) fn parser_settings(&self) -> uqa_sql::parser::ParserSettings {
        let parameters = self.session.parameters.lock();
        let state = self.session.state.read();
        uqa_sql::parser::ParserSettings::from_settings(|name| {
            state
                .session_vars
                .get(name)
                .map(String::as_str)
                .or_else(|| parameters.client_setting(name))
        })
    }

    /// Assign `value` to `name` for the session, as `SET name TO value` does before the assignment's lifetime applies: a defined parameter checks its context and value, and any other valid custom name becomes a placeholder.
    pub(super) fn assign_parameter(&self, name: &str, value: &str) -> Result<(), SQLError> {
        match self.session_execution_view().resolve_parameter(name) {
            Some(SessionParameter::Defined(definition)) => {
                self.check_parameter_change(definition)?;
                if is_transaction_characteristic(definition) {
                    return self.set_transaction_parameter(definition.name, value);
                }
                let setting = self.checked_setting(definition, value)?;
                self.store_setting(definition, Some(setting));
                Ok(())
            }
            Some(SessionParameter::Placeholder(placeholder)) => {
                self.store_placeholder(&placeholder, Some(value.to_string()));
                Ok(())
            }
            None => {
                self.create_placeholder(name)?;
                self.store_placeholder(name, Some(value.to_string()));
                Ok(())
            }
        }
    }

    /// Restore the setting `RESET name` restores; a valid custom name nothing defines becomes an empty placeholder.
    pub(super) fn reset_parameter(&self, name: &str) -> Result<(), SQLError> {
        match self.session_execution_view().resolve_parameter(name) {
            Some(SessionParameter::Defined(definition)) => {
                if definition.has_flag(ParameterFlags::NO_RESET) {
                    return Err(SQLError::Routine {
                        sqlstate: "0A000".into(),
                        message: format!("parameter \"{}\" cannot be reset", definition.name),
                    });
                }
                self.check_parameter_change(definition)?;
                self.store_setting(definition, None);
                Ok(())
            }
            Some(SessionParameter::Placeholder(placeholder)) => {
                self.store_placeholder(&placeholder, None);
                Ok(())
            }
            None => self.create_placeholder(name),
        }
    }

    /// Whether the current role may change `definition` (`set_config_option`'s context checks).
    fn check_parameter_change(&self, definition: &ParameterDefinition) -> Result<(), SQLError> {
        match definition.context {
            ParameterContext::Internal => Err(SQLError::Routine {
                sqlstate: "55P02".into(),
                message: format!("parameter \"{}\" cannot be changed", definition.name),
            }),
            ParameterContext::Superuser if !self.current_user_is_superuser() => {
                Err(SQLError::Routine {
                    sqlstate: "42501".into(),
                    message: format!("permission denied to set parameter \"{}\"", definition.name),
                })
            }
            ParameterContext::Superuser | ParameterContext::User => Ok(()),
        }
    }

    /// Read `value` as a setting of `definition` and apply the parameter's checks, as `parse_and_validate_value` and the check hooks do.
    fn checked_setting(
        &self,
        definition: &ParameterDefinition,
        value: &str,
    ) -> Result<String, SQLError> {
        if let Some(notice) = name_truncation_notice(definition, value) {
            self.push_sql_notice(SQLNotice::notice(notice).with_sqlstate("42622"));
        }
        let setting = parse_setting(definition, value)?;
        match definition.name {
            "application_name" => Ok(clean_ascii(&setting)),
            "search_path" => {
                if split_identifier_list(&setting, b',').is_none() {
                    return Err(invalid_value(
                        definition,
                        &setting,
                        "List syntax is invalid.".into(),
                    ));
                }
                Ok(setting)
            }
            "default_with_oids" if setting == "on" => Err(SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "tables declared WITH OIDS are not supported".into(),
            }),
            "default_tablespace" => {
                if !setting.is_empty() && !TABLESPACES.contains(&setting.as_str()) {
                    return Err(invalid_value(
                        definition,
                        &setting,
                        format!("Tablespace \"{setting}\" does not exist."),
                    ));
                }
                Ok(setting)
            }
            "default_table_access_method" => match access_method_kind(&setting) {
                _ if setting.is_empty() => Err(invalid_value(
                    definition,
                    &setting,
                    "\"default_table_access_method\" cannot be empty.".into(),
                )),
                Some(AccessMethodKind::Table) => Ok(setting),
                Some(AccessMethodKind::Index) => Err(SQLError::Routine {
                    sqlstate: "55000".into(),
                    message: format!("access method \"{setting}\" is not of type TABLE"),
                }),
                None => Err(invalid_value(
                    definition,
                    &setting,
                    format!("Table access method \"{setting}\" does not exist."),
                )),
            },
            _ => Ok(setting),
        }
    }

    /// Keep `setting` as the session's setting of `definition`, or remove the session's setting so that the reset setting applies, with the session state that follows the parameter.
    fn store_setting(&self, definition: &ParameterDefinition, setting: Option<String>) {
        let reset_path = (definition.name == "search_path" && setting.is_none())
            .then(|| self.session_execution_view().reset_setting(definition));
        let key = setting_key(definition.name);
        let mut state = self.session.state.write();
        match definition.name {
            "search_path" => {
                let text = setting
                    .as_deref()
                    .or(reset_path.as_deref())
                    .unwrap_or_default();
                state.search_path =
                    split_identifier_list(text, b',').expect("checked search path syntax");
                state.sql_statement_cache.clear();
            }
            "session_replication_role" => state.sql_statement_cache.clear(),
            _ => {}
        }
        match setting {
            Some(setting) => {
                state.session_vars.insert(key, setting);
            }
            None => {
                state.session_vars.remove(&key);
            }
        }
    }

    fn store_placeholder(&self, name: &str, setting: Option<String>) {
        let key = setting_key(name);
        let mut state = self.session.state.write();
        match setting {
            Some(setting) => {
                state.session_vars.insert(key, setting);
            }
            None => {
                state.session_vars.remove(&key);
            }
        }
    }

    /// Create a placeholder for the custom parameter `name`, which outlives the transaction that creates it.
    fn create_placeholder(&self, name: &str) -> Result<(), SQLError> {
        let mut registry = self.session.parameters.lock();
        check_assignable_custom_name(name, registry.reserved_prefixes())?;
        registry.add_placeholder(name);
        Ok(())
    }

    /// The setting the session's search path takes after `RESET ALL` or `DISCARD ALL`.
    pub(super) fn reset_search_path(&self) -> Vec<String> {
        let definition = uqa_sql::semantics::parameters::catalog::find_parameter("search_path")
            .expect("search_path is defined");
        let setting = self.session_execution_view().reset_setting(definition);
        split_identifier_list(&setting, b',').unwrap_or_else(default_search_path)
    }

    /// `set_config(name, value, is_local)`: assign `value`, or restore the reset setting for `None`, for the session or the transaction, and return the value as `SHOW` reports it.
    pub(crate) fn set_config(
        &self,
        name: &str,
        value: Option<&str>,
        local: bool,
    ) -> Result<String, SQLError> {
        self.set_runtime_parameter(name, value, local)?;
        self.session_execution_view().show_variable(name)
    }

    /// Keep `value` as the client's startup setting of `name`, which the session starts with and `RESET` restores (`PGC_S_CLIENT`).
    pub fn set_client_parameter(&self, name: &str, value: &str) -> Result<(), SQLError> {
        match self.session_execution_view().resolve_parameter(name) {
            Some(SessionParameter::Defined(definition))
                if is_transaction_characteristic(definition)
                    || matches!(definition.name, "role" | "session_authorization") =>
            {
                self.set_variable(definition.name, value)
            }
            Some(SessionParameter::Defined(definition)) => {
                self.check_parameter_change(definition)?;
                let setting = self.checked_setting(definition, value)?;
                if definition.name == "search_path" {
                    self.session.state.write().search_path =
                        split_identifier_list(&setting, b',').expect("checked search path syntax");
                }
                if definition.name == "client_min_messages" {
                    if let Some(level) = crate::state::message_level(&setting) {
                        self.session.state.set_reset_client_level(level);
                    }
                }
                if definition.name == "lock_timeout" {
                    if let Ok(milliseconds) = setting.parse::<u64>() {
                        self.session.state.set_reset_lock_timeout(milliseconds);
                    }
                }
                self.session
                    .parameters
                    .lock()
                    .set_client_setting(definition.name, setting);
                // A client that sets a session timeout at startup is idle from then on, as a backend that waits for its first query is.
                if matches!(
                    definition.name,
                    "idle_session_timeout"
                        | "idle_in_transaction_session_timeout"
                        | "transaction_timeout"
                ) && self.runtime.terminations.is_idle()
                {
                    self.session_became_idle();
                }
                Ok(())
            }
            Some(SessionParameter::Placeholder(placeholder)) => {
                self.store_placeholder(&placeholder, Some(value.to_string()));
                Ok(())
            }
            None => {
                self.create_placeholder(name)?;
                self.store_placeholder(name, Some(value.to_string()));
                Ok(())
            }
        }
    }

    /// Load `library` into the session: its parameters become defined, its prefix is reserved, and each placeholder under the prefix either becomes the setting of the parameter it names or, naming none, is removed with a warning (`MarkGUCPrefixReserved`).
    pub(crate) fn load_parameter_library(&self, library: &'static str) {
        let placeholders = {
            let mut registry = self.session.parameters.lock();
            if !registry.load_library(library) {
                return;
            }
            registry.placeholders_with_prefix(library)
        };
        for placeholder in placeholders {
            let key = setting_key(&placeholder);
            let value = self.session.state.read().session_vars.get(&key).cloned();
            self.session
                .parameters
                .lock()
                .remove_placeholder(&placeholder);
            let definition = uqa_sql::semantics::parameters::catalog::find_parameter(&placeholder)
                .filter(|definition| definition.library == Some(library));
            let Some(definition) = definition else {
                let (message, detail) = reserved_placeholder_warning(&placeholder, library);
                self.push_sql_notice(
                    SQLNotice::warning(message)
                        .with_sqlstate("42602")
                        .with_detail(detail),
                );
                self.session.state.write().session_vars.remove(&key);
                continue;
            };
            let Some(value) = value else {
                continue;
            };
            match self.checked_setting(definition, &value) {
                Ok(setting) => {
                    self.session.state.write().session_vars.insert(key, setting);
                }
                Err(error) => {
                    self.push_sql_notice(
                        SQLNotice::warning(error.to_string())
                            .with_sqlstate(error.sqlstate().unwrap_or("22023")),
                    );
                    self.session.state.write().session_vars.remove(&key);
                }
            }
        }
    }
}
