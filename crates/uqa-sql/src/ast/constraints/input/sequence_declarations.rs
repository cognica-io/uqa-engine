//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced};
use crate::ast::{
    stored_input::{boolean, invalid, number},
    DeferredSQLError, SequenceDeclaration, SequenceOptionValue,
};

impl<'a> Input<'a, '_> {
    pub(super) fn sequence_declaration(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<SequenceDeclaration>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = SequenceDeclaration::default();
        self.fields(
            token,
            &[
                "data_type",
                "increment",
                "cycle",
                "max_value",
                "min_value",
                "start",
                "restart",
                "cache",
                "owned_by",
            ],
            0,
            |input, index, token| {
                match index {
                    0 => value.data_type = parts.take(input.maybe(token, Self::column)?),
                    1 => value.increment = parts.take(input.maybe(token, Self::sequence_option)?),
                    2 if token == JsonToken::Null => (),
                    2 => value.cycle = Some(boolean(token)?),
                    3 => value.max_value = parts.take(input.maybe(token, Self::sequence_option)?),
                    4 => value.min_value = parts.take(input.maybe(token, Self::sequence_option)?),
                    5 => value.start = parts.take(input.maybe(token, Self::sequence_option)?),
                    6 => value.restart = parts.take(input.maybe(token, Self::sequence_option)?),
                    7 => value.cache = parts.take(input.maybe(token, Self::sequence_option)?),
                    _ => {
                        value.owned_by = parts.take(input.maybe(token, |input, token| {
                            input.array(token, |input, token| input.text(token))
                        })?);
                    }
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn sequence_option(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<SequenceOptionValue>, InputError> {
        let (tag, object) = self.tag(token)?;
        let mut parts = Parts::new(self.control);
        let value = if tag.as_str() == "Absent" {
            if object && self.next()? != JsonToken::Null {
                return Err(invalid());
            }
            SequenceOptionValue::Absent
        } else {
            if !object {
                return Err(invalid());
            }
            let token = self.next()?;
            match tag.as_str() {
                "Integer" => SequenceOptionValue::Integer(number(token)?),
                "Text" => SequenceOptionValue::Text(parts.take(self.text(token)?)),
                _ => return Err(invalid()),
            }
        };
        self.close_tag(object)?;
        parts.finish(value)
    }

    pub(super) fn deferred_sql_error(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<DeferredSQLError>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = DeferredSQLError {
            sqlstate: String::new(),
            message: String::new(),
            detail: None,
            hint: None,
        };
        self.fields_with_defaults(
            token,
            &["sqlstate", "message", "detail", "hint"],
            3,
            3,
            |input, index, token| {
                match index {
                    0 => value.sqlstate = parts.take(input.text(token)?),
                    1 => value.message = parts.take(input.text(token)?),
                    2 => value.detail = parts.take(input.optional_text(token)?),
                    _ => value.hint = parts.take(input.optional_text(token)?),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }
}
