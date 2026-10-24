//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! XML input reads the invoking session's option and reports its parser notices through that operation.

use super::{coercion_type_name_with_control, ColumnType, EngineHook, Result};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

pub(super) fn cast(
    value: &Value,
    source_name: Option<&str>,
    source: Option<&ColumnType>,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    let mode = match engine.runtime_parameter("xmloption")? {
        Some(setting) if setting.eq_ignore_ascii_case("document") => {
            crate::xml::XmlOption::Document
        }
        _ => crate::xml::XmlOption::Content,
    };
    let source = source
        .map(|ty| coercion_type_name_with_control(ty, control))
        .transpose()?;
    super::super::super::casting::cast_xml(
        value,
        source.as_deref().map(String::as_str).or(source_name),
        mode,
        control,
        |notice| engine.sql_notice(notice),
    )
}
