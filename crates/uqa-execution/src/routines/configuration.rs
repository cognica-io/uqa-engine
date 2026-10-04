//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalize routine configuration inside a retained caller-state restoration guard.

use uqa_sql::{
    ast::{CreateFunction, RoutineConfigAction},
    routines::configuration::validate_routine_config_action,
    SQLError,
};

/// Dropping the captured guard restores caller variables, namespace, identity, and statement cache.
pub trait RoutineConfigurationGuard {}
pub trait RoutineConfigurationSession {
    fn routine_configuration_guard(&self) -> Box<dyn RoutineConfigurationGuard + '_>;
    fn set_routine_variable(&self, name: &str, value: &str) -> Result<(), SQLError>;
    fn show_routine_variable(&self, name: &str) -> Result<String, SQLError>;
    /// The canonical spelling of the parameter `name` refers to, which a routine's configuration stores.
    fn routine_variable_name(&self, name: &str) -> Result<String, SQLError>;
    /// Load the library of the routine's language into the session, as calling its validator does.
    fn load_language_library(&self, language: &str);
}

pub fn apply_routine_config_actions(
    session: &dyn RoutineConfigurationSession,
    definition: &mut CreateFunction,
) -> Result<(), SQLError> {
    if definition.config_actions.is_empty() {
        return Ok(());
    }
    let _guard = session.routine_configuration_guard();
    let mut result = Ok(());
    for action in std::mem::take(&mut definition.config_actions) {
        let applied = validate_routine_config_action(&action).and_then(|()| match action {
            RoutineConfigAction::Set { name, value } => session
                .set_routine_variable(&name, &value)
                .and_then(|()| session.routine_variable_name(&name))
                .map(|name| Some((name, value))),
            RoutineConfigAction::FromCurrent { name } => session
                .show_routine_variable(&name)
                .and_then(|value| Ok(Some((session.routine_variable_name(&name)?, value)))),
            RoutineConfigAction::Reset { name } => {
                definition
                    .config
                    .retain(|(existing, _)| !existing.eq_ignore_ascii_case(&name));
                Ok(None)
            }
            RoutineConfigAction::ResetAll => {
                definition.config.clear();
                Ok(None)
            }
        });
        match applied {
            Ok(Some((name, value))) => {
                if let Some((_, existing)) = definition
                    .config
                    .iter_mut()
                    .find(|(existing, _)| existing.eq_ignore_ascii_case(&name))
                {
                    *existing = value;
                } else {
                    definition.config.push((name, value));
                }
            }
            Ok(None) => {}
            Err(error) => {
                result = Err(error);
                break;
            }
        }
    }
    result
}
