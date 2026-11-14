//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed field slices preserve duplicate rejection and original positional defaults.

use super::{
    invalid, json_error, retained, take, Budgeted, ProductionControl, StorageBackendResult,
};
use uqa_core::{
    json::{decode_json_string_with_control, JsonReader, JsonToken},
    memory::{BudgetedVec, OwnedMap},
};

pub(super) struct Reader<'a, 'c> {
    reader: JsonReader<'a, 'c>,
    text: &'a str,
    range: std::ops::Range<usize>,
    control: ProductionControl<'c>,
}

impl<'a, 'c> Reader<'a, 'c> {
    pub(super) fn new(text: &'a str, control: ProductionControl<'c>) -> Self {
        Self {
            reader: JsonReader::with_control(text, &control).with_depth_limit(127),
            text,
            range: 0..0,
            control,
        }
    }

    pub(super) fn next(&mut self) -> StorageBackendResult<JsonToken<'a>> {
        let event = self
            .reader
            .next_event()
            .map_err(json_error)?
            .ok_or_else(invalid)?;
        self.range = event.range;
        Ok(event.token)
    }

    fn encoded(&mut self, first: JsonToken<'a>) -> StorageBackendResult<&'a str> {
        let start = self.range.start;
        let mut depth = usize::from(matches!(
            first,
            JsonToken::StartArray | JsonToken::StartObject
        ));
        while depth != 0 {
            match self.next()? {
                JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                _ => (),
            }
        }
        Ok(&self.text[start..self.range.end])
    }

    pub(super) fn end(&mut self) -> StorageBackendResult<()> {
        if self.reader.next_event().map_err(json_error)?.is_some() {
            return Err(invalid());
        }
        self.control.check()?;
        Ok(())
    }

    pub(super) fn fields<const N: usize>(
        &mut self,
        names: [&str; N],
        required: &[usize],
        deny_unknown: bool,
    ) -> StorageBackendResult<[Option<&'a str>; N]> {
        let object = match self.next()? {
            JsonToken::StartObject => true,
            JsonToken::StartArray => false,
            _ => return Err(invalid()),
        };
        let mut fields = [None; N];
        let mut position = 0;
        loop {
            let token = self.next()?;
            if matches!(
                (object, token),
                (true, JsonToken::EndObject) | (false, JsonToken::EndArray)
            ) {
                break;
            }
            let (index, token) = if object {
                let JsonToken::Key(key) = token else {
                    return Err(invalid());
                };
                let key =
                    decode_json_string_with_control(key, &self.control).map_err(json_error)?;
                let index = names.iter().position(|name| *name == key.as_str());
                if index.is_none() && deny_unknown {
                    return Err(invalid());
                }
                (index, self.next()?)
            } else {
                if position == N {
                    return Err(invalid());
                }
                let index = position;
                position += 1;
                (Some(index), token)
            };
            let text = self.encoded(token)?;
            if let Some(index) = index {
                if fields[index].replace(text).is_some() {
                    return Err(invalid());
                }
            }
        }
        if required.iter().any(|index| fields[*index].is_none()) {
            return Err(invalid());
        }
        self.end()?;
        Ok(fields)
    }
}

pub(super) fn text(
    text: &str,
    control: ProductionControl<'_>,
) -> StorageBackendResult<Budgeted<String>> {
    let mut reader = Reader::new(text, control);
    let JsonToken::String(text) = reader.next()? else {
        return Err(invalid());
    };
    let value = retained(decode_json_string_with_control(text, &control).map_err(json_error)?);
    reader.end()?;
    Ok(value)
}

pub(super) fn number<T: std::str::FromStr>(token: JsonToken<'_>) -> StorageBackendResult<T> {
    let JsonToken::Number(text) = token else {
        return Err(invalid());
    };
    // Generic content retains integer signed zero as an integer, while fractional/exponent forms remain private-number maps.
    let text = if text == "-0" { "0" } else { text };
    text.parse().map_err(|_| invalid())
}

pub(super) fn integer<T: std::str::FromStr>(
    text: &str,
    control: ProductionControl<'_>,
) -> StorageBackendResult<T> {
    let mut reader = Reader::new(text, control);
    let value = number(reader.next()?)?;
    reader.end()?;
    Ok(value)
}

pub(super) fn identity(
    text: &str,
    control: ProductionControl<'_>,
) -> StorageBackendResult<[u8; 16]> {
    let mut reader = Reader::new(text, control);
    if reader.next()? != JsonToken::StartArray {
        return Err(invalid());
    }
    let mut value = [0; 16];
    for byte in &mut value {
        *byte = number(reader.next()?)?;
    }
    if reader.next()? != JsonToken::EndArray {
        return Err(invalid());
    }
    reader.end()?;
    Ok(value)
}

pub(super) fn texts(
    text: &str,
    control: ProductionControl<'_>,
) -> StorageBackendResult<Budgeted<Vec<String>>> {
    let budget = control.budget().expect("controlled foreign input");
    let mut memory = budget.empty_reservation();
    let mut values = BudgetedVec::new(budget);
    let mut reader = Reader::new(text, control);
    if reader.next()? != JsonToken::StartArray {
        return Err(invalid());
    }
    loop {
        let token = reader.next()?;
        if token == JsonToken::EndArray {
            break;
        }
        let JsonToken::String(text) = token else {
            return Err(invalid());
        };
        let value = retained(decode_json_string_with_control(text, &control).map_err(json_error)?);
        values.push(take(value, &mut memory))?;
    }
    reader.end()?;
    let (values, lease) = values.into_parts();
    memory.absorb(lease);
    Ok(Budgeted::new(values, memory))
}

pub(super) fn options(
    text: &str,
    control: ProductionControl<'_>,
) -> StorageBackendResult<Budgeted<OwnedMap<String, String>>> {
    let mut memory = control
        .budget()
        .expect("controlled foreign input")
        .empty_reservation();
    let mut values = OwnedMap::<String, String>::new();
    let mut reader = Reader::new(text, control);
    if reader.next()? != JsonToken::StartObject {
        return Err(invalid());
    }
    loop {
        let token = reader.next()?;
        if token == JsonToken::EndObject {
            break;
        }
        let JsonToken::Key(key) = token else {
            return Err(invalid());
        };
        let key = retained(decode_json_string_with_control(key, &control).map_err(json_error)?);
        let JsonToken::String(value) = reader.next()? else {
            return Err(invalid());
        };
        let value = retained(decode_json_string_with_control(value, &control).map_err(json_error)?);
        if let Some(previous) = values.get_mut(key.as_str()) {
            let bytes = previous.capacity();
            *previous = take(value, &mut memory);
            drop(memory.split(bytes));
        } else {
            memory.grow(OwnedMap::<String, String>::entry_bytes())?;
            values.insert(take(key, &mut memory), take(value, &mut memory));
        }
    }
    reader.end()?;
    Ok(Budgeted::new(values, memory))
}
