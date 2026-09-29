//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `ExecBuildSlotValueDescription`: the row that a NOT NULL, CHECK, partition or view check violation reports in DETAIL. A role with table-level SELECT sees every column; otherwise it sees the columns it may select or supplied values for, named before their values, and no description when no column qualifies.

use std::collections::BTreeSet;

use crate::ast::{ColumnDef, GeneratedColumnKind};
use crate::expr::EngineHook;
use crate::result::format_postgres_text;
use crate::{ResultRow, SQLError};
use uqa_core::Value;

/// Output text longer than this many bytes is clipped and marked with `...`.
pub const MAX_DESCRIBED_FIELD_BYTES: usize = 64;

/// The columns of a relation that the current role may see in a failing-row description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowDescriptionAccess {
    /// Table-level SELECT: every column, without a column list.
    Table,
    /// Only these columns, listed by name before their values.
    Columns(BTreeSet<String>),
}

/// `(v1, v2, ...)`, or `(c1, c2) = (v1, v2)` for column-level access, in column order. Virtual generated columns print `virtual` and NULL prints `null`.
pub fn failing_row_description(
    engine: Option<&dyn EngineHook>,
    access: &RowDescriptionAccess,
    columns: &[ColumnDef],
    row: &ResultRow,
) -> Result<Option<String>, SQLError> {
    let mut names = Vec::new();
    let mut values = Vec::new();
    for column in columns {
        if let RowDescriptionAccess::Columns(visible) = access {
            if !visible.contains(&column.name) {
                continue;
            }
            names.push(column.name.as_str());
        }
        let virtual_column = column
            .generated
            .as_ref()
            .is_some_and(|generated| generated.kind == GeneratedColumnKind::Virtual);
        values.push(if virtual_column {
            "virtual".to_string()
        } else {
            match row.get(&column.name) {
                None | Some(Value::Null) => "null".to_string(),
                Some(value) => clip_field(format_postgres_text(value, &column.ty, engine)?),
            }
        });
    }
    Ok(match access {
        RowDescriptionAccess::Table => Some(format!("({})", values.join(", "))),
        RowDescriptionAccess::Columns(_) if names.is_empty() => None,
        RowDescriptionAccess::Columns(_) => {
            Some(format!("({}) = ({})", names.join(", "), values.join(", ")))
        }
    })
}

/// `pg_mbcliplen` to the field limit on a character boundary, then `...`.
pub fn clip_field(mut text: String) -> String {
    if text.len() <= MAX_DESCRIBED_FIELD_BYTES {
        return text;
    }
    let mut end = MAX_DESCRIBED_FIELD_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str("...");
    text
}

#[cfg(test)]
mod tests {
    use super::{clip_field, failing_row_description, RowDescriptionAccess};
    use crate::ast::{ColumnDef, ColumnType};
    use uqa_core::Value;

    #[test]
    fn long_output_is_clipped_on_a_character_boundary() {
        assert_eq!(clip_field("short".into()), "short");
        let exact = "a".repeat(64);
        assert_eq!(clip_field(exact.clone()), exact);
        assert_eq!(clip_field("b".repeat(65)), format!("{}...", "b".repeat(64)));
        // 63 ASCII bytes followed by a three-byte character: the character does not fit.
        let text = format!("{}\u{d55c}", "c".repeat(63));
        assert_eq!(clip_field(text), format!("{}...", "c".repeat(63)));
    }

    #[test]
    fn column_access_lists_visible_columns_before_their_values() {
        let columns = vec![
            ColumnDef::nullable("id", ColumnType::Integer),
            ColumnDef::nullable("secret", ColumnType::Text),
            ColumnDef::nullable("note", ColumnType::Text),
        ];
        let row = [
            ("id".to_string(), Value::Int(7)),
            ("secret".to_string(), Value::Str("x".into())),
            ("note".to_string(), Value::Null),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            failing_row_description(None, &RowDescriptionAccess::Table, &columns, &row).unwrap(),
            Some("(7, x, null)".into())
        );
        let visible = ["id", "note"].into_iter().map(String::from).collect();
        assert_eq!(
            failing_row_description(
                None,
                &RowDescriptionAccess::Columns(visible),
                &columns,
                &row
            )
            .unwrap(),
            Some("(id, note) = (7, null)".into())
        );
        assert_eq!(
            failing_row_description(
                None,
                &RowDescriptionAccess::Columns(std::collections::BTreeSet::default()),
                &columns,
                &row
            )
            .unwrap(),
            None
        );
    }
}
