//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_engine::Engine;
use uqa_pg_wire::StartupMessage;
use uqa_sql::SQLError;

/// The parameters whose changes the server reports to the client in `ParameterStatus` messages (`GUC_REPORT`).
pub(crate) fn reported_parameters() -> impl Iterator<Item = &'static str> {
    use uqa_sql::semantics::parameters::{
        catalog::parameter_definitions, definition::ParameterFlags,
    };
    parameter_definitions()
        .iter()
        .filter(|definition| definition.has_flag(ParameterFlags::REPORT))
        .map(|definition| definition.name)
}

pub(crate) fn configure(engine: &Engine, startup: &StartupMessage) -> Result<(), SQLError> {
    for (name, value) in &startup.parameter_pairs {
        match name.as_str() {
            "user" | "database" => {}
            name if name.starts_with("_pq_.") => {}
            "options" => {
                for (name, value) in parse_options(value)? {
                    engine.set_client_parameter(&name, &value)?;
                }
            }
            _ => engine.set_client_parameter(name, value)?,
        }
    }
    Ok(())
}

fn parse_options(text: &str) -> Result<Vec<(String, String)>, SQLError> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in text.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character.is_ascii_whitespace() {
            if !current.is_empty() {
                arguments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    let mut arguments = arguments.into_iter();
    let mut settings = Vec::new();
    while let Some(argument) = arguments.next() {
        let setting = if argument == "-c" {
            arguments.next().ok_or_else(invalid_options)?
        } else if let Some(setting) = argument
            .strip_prefix("-c")
            .or_else(|| argument.strip_prefix("--"))
        {
            setting.to_string()
        } else {
            return Err(invalid_options());
        };
        let (name, value) = setting.split_once('=').ok_or_else(invalid_options)?;
        settings.push((name.replace('-', "_"), value.to_string()));
    }
    Ok(settings)
}

fn invalid_options() -> SQLError {
    SQLError::Routine {
        sqlstate: "42601".into(),
        message: "invalid command-line argument for server process".into(),
    }
}
