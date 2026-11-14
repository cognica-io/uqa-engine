//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken};

impl<'a> Input<'a, '_> {
    pub fn fields(
        &mut self,
        first: JsonToken<'a>,
        names: &[&str],
        required: u64,
        field: impl FnMut(&mut Self, usize, JsonToken<'a>) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        self.fields_with_defaults(first, names, required, (1 << names.len()) - 1, field)
    }

    /// Struct maps ignore unknown values; sequence forms retain every declared positional field.
    pub fn fields_with_defaults(
        &mut self,
        first: JsonToken<'a>,
        names: &[&str],
        required: u64,
        sequence_required: u64,
        field: impl FnMut(&mut Self, usize, JsonToken<'a>) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        self.field_set(first, names, [required, sequence_required], false, field)
    }

    pub fn closed_fields(
        &mut self,
        first: JsonToken<'a>,
        names: &[&str],
        required: [u64; 2],
        field: impl FnMut(&mut Self, usize, JsonToken<'a>) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        self.field_set(first, names, required, true, field)
    }

    fn field_set(
        &mut self,
        first: JsonToken<'a>,
        names: &[&str],
        [required, sequence_required]: [u64; 2],
        reject_unknown: bool,
        mut field: impl FnMut(&mut Self, usize, JsonToken<'a>) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        let mut cursor =
            self.field_cursor(first, names, [required, sequence_required], reject_unknown)?;
        while let Some((index, token)) = cursor.next(self)? {
            field(self, index, token)?;
        }
        Ok(())
    }

    pub(super) fn content_number_fields(&self, token: JsonToken<'_>) -> bool {
        let JsonToken::Number(text) = token else {
            return false;
        };
        self.content_numbers
            && if text.starts_with('-') {
                text.len() > 20 || text.parse::<i64>().is_err()
            } else {
                text.len() > 20 || text.parse::<u64>().is_err()
            }
    }
}
