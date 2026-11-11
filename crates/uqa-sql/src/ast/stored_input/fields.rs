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
        mut field: impl FnMut(&mut Self, usize, JsonToken<'a>) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
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
}
