//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statistics retain one table prefix and its original committed/private view.

use super::{KeyValueCatalog, TAG_COLUMN_STATS};
use crate::{
    catalog::input::{statistics, strings, CatalogInput, CatalogInputLimits},
    key_value::codec::{read_str_ref, single_str_key_prefix},
    read_control::StorageReadControl,
    ColumnStatsRow, StorageBackendError, StorageBackendResult,
};
use uqa_core::{
    json::{decode_json_string, JsonToken},
    memory::{Budgeted, ProductionControl},
};

impl KeyValueCatalog {
    pub(super) fn column_stats_input(
        &self,
        table: &str,
        limits: CatalogInputLimits,
        control: &StorageReadControl,
    ) -> StorageBackendResult<CatalogInput<ColumnStatsRow>> {
        control.check()?;
        let size = table.len().checked_add(5).ok_or_else(|| {
            StorageBackendError::Other("column statistics prefix size overflow".into())
        })?;
        control.check_value_size(size, limits.maximum_key_bytes)?;
        let prefix = single_str_key_prefix(TAG_COLUMN_STATS, table, table.len(), control.memory())?;
        self.row_input(&prefix, decode, limits, control)
    }
}

#[derive(Default)]
enum Field {
    #[default]
    Absent,
    Integer(i64),
    Text(Budgeted<String>),
}

impl Field {
    fn integer(self) -> i64 {
        let Self::Integer(value) = self else {
            unreachable!("required integer field")
        };
        value
    }

    fn text(self) -> Option<Budgeted<String>> {
        match self {
            Self::Text(value) => Some(value),
            Self::Absent => None,
            Self::Integer(_) => unreachable!("declared string field"),
        }
    }
}

fn decode(
    key: &[u8],
    value: &[u8],
    control: &StorageReadControl,
) -> StorageBackendResult<Option<Budgeted<ColumnStatsRow>>> {
    let mut offset = 1;
    let _table = read_str_ref(key, &mut offset)?;
    let column = read_str_ref(key, &mut offset)?;
    let column = ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
    .copy_text(column)?
    .into_budgeted()
    .expect("catalog input retains an allowance");
    let names = [
        ("distinct_count", false),
        ("null_count", false),
        ("min_value", true),
        ("max_value", true),
        ("row_count", false),
        ("histogram_json", false),
        ("mcv_values_json", false),
        ("mcv_frequencies_json", false),
    ];
    let [distinct, nulls, min, max, rows, histogram, values, frequencies] =
        strings::fields_with(value, names, control, |index, token| {
            if matches!(index, 0 | 1 | 4) {
                let JsonToken::Number(number) = token else {
                    return Err(strings::invalid(format!(
                        "invalid integer field `{}`",
                        names[index].0
                    )));
                };
                return serde_json::from_str(number)
                    .map(Field::Integer)
                    .map_err(Into::into);
            }
            match token {
                JsonToken::String(text) => {
                    decode_json_string(text, control.memory(), control.cancellation())
                        .map(Field::Text)
                        .map_err(strings::json_error)
                }
                JsonToken::Null if names[index].1 => Ok(Field::Absent),
                _ => Err(strings::invalid(format!(
                    "invalid string field `{}`",
                    names[index].0
                ))),
            }
        })?;
    Ok(Some(statistics::row(
        [
            column,
            histogram.text().expect("required field"),
            values.text().expect("required field"),
            frequencies.text().expect("required field"),
        ],
        [min.text(), max.text()],
        [distinct.integer(), nulls.integer(), rows.integer()],
    )))
}

#[cfg(test)]
mod tests;
