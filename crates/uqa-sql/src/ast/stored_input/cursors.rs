//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Container cursors retain declaration position while a native child constructor is pending.

use super::{invalid, Input, InputError, JsonToken, Produced};

pub(crate) struct ArrayCursor {
    complete: bool,
}

pub(crate) struct ObjectCursor {
    complete: bool,
}

pub(crate) struct FieldCursor<'n> {
    names: &'n [&'n str],
    required: [u64; 2],
    seen: u64,
    reject_unknown: bool,
    container: FieldContainer,
}

enum FieldContainer {
    Object(ObjectCursor),
    Array(ArrayCursor, usize),
    Complete,
}

impl<'a> Input<'a, '_> {
    pub(crate) fn array_cursor(&mut self, first: JsonToken<'a>) -> Result<ArrayCursor, InputError> {
        if first != JsonToken::StartArray {
            return Err(invalid());
        }
        self.enter()?;
        Ok(ArrayCursor { complete: false })
    }

    pub(crate) fn object_cursor(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<ObjectCursor, InputError> {
        if first != JsonToken::StartObject {
            return Err(invalid());
        }
        self.enter()?;
        Ok(ObjectCursor { complete: false })
    }

    pub(crate) fn field_cursor<'n>(
        &mut self,
        first: JsonToken<'a>,
        names: &'n [&'n str],
        required: [u64; 2],
        reject_unknown: bool,
    ) -> Result<FieldCursor<'n>, InputError> {
        let container = if self.content_number_fields(first) {
            if reject_unknown || required[0] != 0 {
                return Err(invalid());
            }
            self.control.check()?;
            FieldContainer::Complete
        } else {
            match first {
                JsonToken::StartObject => FieldContainer::Object(self.object_cursor(first)?),
                JsonToken::StartArray => FieldContainer::Array(self.array_cursor(first)?, 0),
                _ => return Err(invalid()),
            }
        };
        Ok(FieldCursor {
            names,
            required,
            seen: 0,
            reject_unknown,
            container,
        })
    }
}

impl ArrayCursor {
    pub(crate) fn next<'a>(
        &mut self,
        input: &mut Input<'a, '_>,
    ) -> Result<Option<JsonToken<'a>>, InputError> {
        if self.complete {
            return Ok(None);
        }
        let token = input.next()?;
        if token == JsonToken::EndArray {
            self.complete = true;
            input.remaining_depth += 1;
            Ok(None)
        } else {
            Ok(Some(token))
        }
    }
}

impl ObjectCursor {
    pub(crate) fn next<'a>(
        &mut self,
        input: &mut Input<'a, '_>,
    ) -> Result<Option<(Produced<String>, JsonToken<'a>)>, InputError> {
        let Some(key) = self.key(input)? else {
            return Ok(None);
        };
        Ok(Some((key, input.next()?)))
    }

    fn key(&mut self, input: &mut Input<'_, '_>) -> Result<Option<Produced<String>>, InputError> {
        if self.complete {
            return Ok(None);
        }
        let token = input.next()?;
        if token == JsonToken::EndObject {
            self.complete = true;
            input.remaining_depth += 1;
            return Ok(None);
        }
        if !matches!(token, JsonToken::Key(_)) {
            return Err(invalid());
        }
        Ok(Some(input.text(token)?))
    }
}

impl FieldCursor<'_> {
    pub(crate) fn next<'a>(
        &mut self,
        input: &mut Input<'a, '_>,
    ) -> Result<Option<(usize, JsonToken<'a>)>, InputError> {
        loop {
            match &mut self.container {
                FieldContainer::Complete => return Ok(None),
                FieldContainer::Object(cursor) => {
                    let Some(name) = cursor.key(input)? else {
                        if self.seen & self.required[0] != self.required[0] {
                            return Err(invalid());
                        }
                        self.container = FieldContainer::Complete;
                        return Ok(None);
                    };
                    if let Some(index) = self.names.iter().position(|field| *field == name.as_str())
                    {
                        let bit = 1 << index;
                        if self.seen & bit != 0 {
                            return Err(invalid());
                        }
                        self.seen |= bit;
                        return Ok(Some((index, input.next()?)));
                    }
                    if self.reject_unknown {
                        return Err(invalid());
                    }
                    let token = input.next()?;
                    input.skip(token)?;
                }
                FieldContainer::Array(cursor, index) => {
                    let Some(token) = cursor.next(input)? else {
                        let required = self.required[0] | self.required[1];
                        if self.seen & required != required {
                            return Err(invalid());
                        }
                        self.container = FieldContainer::Complete;
                        return Ok(None);
                    };
                    if *index == self.names.len() {
                        return Err(invalid());
                    }
                    let position = *index;
                    self.seen |= 1 << position;
                    *index += 1;
                    return Ok(Some((position, token)));
                }
            }
        }
    }
}
