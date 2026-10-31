//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `SHOW name` and `SHOW ALL`, as `PostgreSQL`'s `GetPGVariable` reports them: one text column named after the parameter's canonical spelling, or the name, setting and description of every parameter that `SHOW ALL` lists.

use crate::catalog::services::CatalogSession;
use uqa_core::Value;
use uqa_sql::{ColumnType, ResultRow, SQLError, SQLResult, SQLResultKind};

const SHOW_ALL_COLUMNS: [&str; 3] = ["name", "setting", "description"];

fn shows_all(name: &str) -> bool {
    name.eq_ignore_ascii_case("all")
}

/// The columns `SHOW name` returns.
pub fn show_columns(session: &dyn CatalogSession, name: &str) -> Result<Vec<String>, SQLError> {
    if shows_all(name) {
        return Ok(SHOW_ALL_COLUMNS.map(str::to_string).to_vec());
    }
    session.show_parameter(name).map(|(column, _)| vec![column])
}

pub fn show_schema(
    session: &dyn CatalogSession,
    name: &str,
) -> Result<uqa_sql::RowSchema, SQLError> {
    show_schema_with_control(
        session,
        name,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn show_schema_with_control(
    session: &dyn CatalogSession,
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<uqa_sql::RowSchema, SQLError> {
    control.check()?;
    let ty = ColumnType::Text;
    if shows_all(name) {
        return uqa_sql::RowSchema::with_column_metadata_with_control(
            None,
            SHOW_ALL_COLUMNS
                .into_iter()
                .map(|name| (name, Some(&ty), None)),
            control,
        );
    }
    let name = session.parameter_name_with_control(name, control)?;
    uqa_sql::RowSchema::with_column_metadata_with_control(
        None,
        [(name.as_str(), Some(&ty), None)],
        control,
    )
}

/// The result of `SHOW name`.
pub fn show(session: &dyn CatalogSession, name: &str) -> Result<SQLResult, SQLError> {
    let (columns, rows) = if shows_all(name) {
        let rows = session
            .parameter_settings()
            .into_iter()
            .map(|setting| {
                let mut row = ResultRow::new();
                row.insert("name".into(), Value::Str(setting.definition.name.into()));
                row.insert("setting".into(), Value::Str(setting.shown()));
                row.insert(
                    "description".into(),
                    Value::Str(setting.definition.short_desc.into()),
                );
                row
            })
            .collect::<Vec<_>>();
        (SHOW_ALL_COLUMNS.map(str::to_string).to_vec(), rows)
    } else {
        let (column, value) = session.show_parameter(name)?;
        let mut row = ResultRow::new();
        row.insert(column.clone(), Value::Str(value));
        (vec![column], vec![row])
    };
    Ok(SQLResult {
        kind: SQLResultKind::Rows,
        command_tag: None,
        row_schema: None,
        column_types: vec![Some(ColumnType::Text); columns.len()],
        columns,
        rows,
        positional_rows: None,
        affected_rows: 0,
    })
}
