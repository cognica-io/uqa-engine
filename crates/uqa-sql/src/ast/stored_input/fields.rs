//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken};

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
        if self.content_number_fields(first) {
            // Arbitrary-precision generic content represents non-native numbers as an object with one unknown field. All declared fields must therefore be optional/defaulted; a closed struct still rejects it. This representation creates no additional JSON container.
            return if !reject_unknown && required == 0 {
                self.control.check().map_err(Into::into)
            } else {
                Err(invalid())
            };
        }
        self.enter()?;
        let mut seen = 0_u64;
        match first {
            JsonToken::StartObject => loop {
                let token = self.next()?;
                if token == JsonToken::EndObject {
                    break;
                }
                let JsonToken::Key(_) = token else {
                    return Err(invalid());
                };
                let name = self.text(token)?;
                if let Some(index) = names
                    .iter()
                    .position(|candidate| *candidate == name.as_str())
                {
                    let bit = 1 << index;
                    if seen & bit != 0 {
                        return Err(invalid());
                    }
                    seen |= bit;
                    let token = self.next()?;
                    field(self, index, token)?;
                } else {
                    if reject_unknown {
                        return Err(invalid());
                    }
                    let token = self.next()?;
                    self.skip(token)?;
                }
            },
            JsonToken::StartArray => {
                let mut index = 0;
                loop {
                    let token = self.next()?;
                    if token == JsonToken::EndArray {
                        if seen & sequence_required != sequence_required {
                            return Err(invalid());
                        }
                        break;
                    }
                    if index == names.len() {
                        return Err(invalid());
                    }
                    field(self, index, token)?;
                    seen |= 1 << index;
                    index += 1;
                }
            }
            _ => return Err(invalid()),
        }
        if seen & required != required {
            return Err(invalid());
        }
        self.remaining_depth += 1;
        Ok(())
    }

    fn content_number_fields(&self, token: JsonToken<'_>) -> bool {
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
