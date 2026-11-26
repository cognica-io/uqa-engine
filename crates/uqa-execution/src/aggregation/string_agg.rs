//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The `string_agg` transition retains the size of every delimiter, including the first. Final output starts after that first delimiter.

use uqa_core::Value;
use uqa_sql::{
    expr::{
        text_size::{check_buffer_append, check_varlena_size},
        SQLValueCatalog,
    },
    SQLError,
};

#[derive(Default)]
pub(super) struct Transition {
    bytes: usize,
    binary: Option<bool>,
}

impl Transition {
    pub(super) fn accept(
        &mut self,
        pair: &Value,
        catalog: Option<&dyn SQLValueCatalog>,
        mut emit: impl FnMut(&[u8]),
    ) -> Result<(), SQLError> {
        let Value::List(pair) = pair else {
            return Err(invalid_pair());
        };
        let [value, delimiter] = pair.as_slice() else {
            return Err(invalid_pair());
        };
        if matches!(value, Value::Null) {
            return Ok(());
        }
        let value = super::observation::read(value, catalog)?;
        let binary = self.binary.unwrap_or(matches!(&*value, Value::Bytes(_)));
        let delimiter = super::observation::read(delimiter, catalog)?;
        let delimiter = bytes(&delimiter, binary)?;
        let value = bytes(&value, binary)?;
        check_buffer_append(self.bytes, delimiter.len())?;
        let after_delimiter = self.bytes + delimiter.len();
        check_buffer_append(after_delimiter, value.len())?;
        if self.binary.is_some() {
            emit(delimiter);
        }
        emit(value);
        self.binary = Some(binary);
        self.bytes = after_delimiter + value.len();
        Ok(())
    }

    fn finish(&self, output: Vec<u8>) -> Result<Value, SQLError> {
        check_varlena_size(output.len())?;
        match self.binary {
            None => Ok(Value::Null),
            Some(true) => Ok(Value::Bytes(output)),
            Some(false) => String::from_utf8(output).map(Value::Str).map_err(|error| {
                SQLError::Internal(format!("string_agg produced invalid UTF-8: {error}"))
            }),
        }
    }
}

fn invalid_pair() -> SQLError {
    SQLError::Internal("string_agg lost its value and delimiter pair".into())
}

fn bytes(value: &Value, binary: bool) -> Result<&[u8], SQLError> {
    match value {
        Value::Null => Ok(&[]),
        Value::Bytes(value) if binary => Ok(value),
        Value::Str(value) if !binary => Ok(value.as_bytes()),
        Value::FixedChar(value) if !binary => Ok(value.trim_end_matches(' ').as_bytes()),
        value => Err(SQLError::TypeMismatch(format!(
            "string_agg requires {} inputs, got {value:?}",
            if binary { "bytea" } else { "text" }
        ))),
    }
}

pub(super) fn finish(
    values: &super::AggregateValueBuffer,
    catalog: Option<&dyn SQLValueCatalog>,
) -> Result<Value, SQLError> {
    let mut state = Transition::default();
    let mut output = Vec::new();
    values.for_each_ordered(catalog, |record| {
        state.accept(&record.value, catalog, |chunk| {
            output.extend_from_slice(chunk);
        })
    })?;
    state.finish(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiter_counts_before_value_and_failed_transition_keeps_its_state() {
        let mut state = Transition {
            bytes: 1_073_741_819,
            binary: Some(false),
        };
        let pair = Value::List(vec![Value::Str("中".into()), Value::Str("a".into())]);
        let error = state
            .accept(&pair, None, |_| panic!("failed transition emitted a value"))
            .unwrap_err();
        assert_eq!(error.sqlstate(), Some("54000"));
        assert_eq!(
            error.detail(),
            Some("Cannot enlarge string buffer containing 1073741820 bytes by 3 more bytes.")
        );
        assert_eq!(state.bytes, 1_073_741_819);
    }
}
