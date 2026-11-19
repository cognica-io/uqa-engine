//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native authority fields after the enclosing owner validates the complete generic JSON value.

use crate::ast::stored_input::{invalid, InputError};
use uqa_core::{
    catalog_role::{BoundAclEntry, RoleIdentity},
    json::{decode_json_string_with_control, JsonObjectProjection, JsonReader, JsonToken},
    memory::{Produced, ProductionControl, ProductionVec},
};

/// Field tags observed through the installed public Serde scalar serializers.
#[derive(Clone, Copy)]
pub struct ScalarProtocol {
    pub number: &'static str,
    pub raw: &'static str,
}

pub(crate) struct Input<'c> {
    pub control: ProductionControl<'c>,
    pub protocol: ScalarProtocol,
}

struct Source<'a> {
    original: &'a [u8],
    decoded: Option<Produced<String>>,
}
impl Source<'_> {
    fn bytes(&self) -> &[u8] {
        self.decoded
            .as_ref()
            .map_or(self.original, |text| text.as_bytes())
    }
}

impl Input<'_> {
    fn source<'a>(&self, bytes: &'a [u8]) -> Result<Source<'a>, InputError> {
        let mut source = Source {
            original: bytes,
            decoded: None,
        };
        while let Some((text, number)) = self.scalar(source.bytes())? {
            source.decoded = Some(text);
            if number {
                break;
            }
        }
        Ok(source)
    }

    fn scalar(&self, bytes: &[u8]) -> Result<Option<(Produced<String>, bool)>, InputError> {
        self.control.check()?;
        let mut reader = JsonReader::from_slice_with_control(bytes, &self.control);
        if reader.next_event()?.map(|event| event.token) != Some(JsonToken::StartObject) {
            return Ok(None);
        }
        let Some(JsonToken::Key(key)) = reader.next_event()?.map(|event| event.token) else {
            return Ok(None);
        };
        let key = decode_json_string_with_control(key, &self.control)?;
        let number = key.as_str() == self.protocol.number;
        if !number && key.as_str() != self.protocol.raw {
            return Ok(None);
        }
        let Some(JsonToken::String(text)) = reader.next_event()?.map(|event| event.token) else {
            return Err(invalid());
        };
        let text = decode_json_string_with_control(text, &self.control)?;
        if reader.next_event()?.map(|event| event.token) != Some(JsonToken::EndObject)
            || reader.next_event()?.is_some()
        {
            return Err(invalid());
        }
        if number {
            let mut number_reader = JsonReader::with_control(&text, &self.control);
            let event = number_reader.next_event()?.ok_or_else(invalid)?;
            if !matches!(event.token, JsonToken::Number(_))
                || event.range != (0..text.len())
                || number_reader.next_event()?.is_some()
            {
                return Err(invalid());
            }
        }
        Ok(Some((text, number)))
    }

    pub fn fields(
        &self,
        bytes: &[u8],
        names: &[&str],
        required: [u64; 2],
        closed: bool,
        mut field: impl FnMut(usize, &[u8]) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        let source = self.source(bytes)?;
        let mut reader = JsonReader::from_slice_with_control(source.bytes(), &self.control);
        let first = reader.next_event()?.ok_or_else(invalid)?.token;
        let mut seen = 0;
        let required = match first {
            JsonToken::StartObject => {
                let object =
                    JsonObjectProjection::read_with_control(source.bytes(), &self.control)?;
                for (name, bytes) in object.iter() {
                    self.control.check()?;
                    if let Some(index) = names.iter().position(|candidate| *candidate == name) {
                        field(index, bytes)?;
                        seen |= 1 << index;
                    } else if closed {
                        return Err(invalid());
                    }
                }
                required[0]
            }
            JsonToken::StartArray => {
                let mut index = 0;
                self.array(source.bytes(), |bytes| {
                    if index == names.len() {
                        return Err(invalid());
                    }
                    field(index, bytes)?;
                    seen |= 1 << index;
                    index += 1;
                    Ok(())
                })?;
                required[1]
            }
            _ => return Err(invalid()),
        };
        if seen & required != required {
            return Err(invalid());
        }
        self.control.check()?;
        Ok(())
    }

    pub fn array(
        &self,
        bytes: &[u8],
        mut item: impl FnMut(&[u8]) -> Result<(), InputError>,
    ) -> Result<(), InputError> {
        let source = self.source(bytes)?;
        let bytes = source.bytes();
        let mut reader = JsonReader::from_slice_with_control(bytes, &self.control);
        if reader.next_event()?.map(|event| event.token) != Some(JsonToken::StartArray) {
            return Err(invalid());
        }
        loop {
            let event = reader.next_event()?.ok_or_else(invalid)?;
            if event.token == JsonToken::EndArray {
                break;
            }
            let start = event.range.start;
            let mut end = event.range.end;
            let mut depth = usize::from(matches!(
                event.token,
                JsonToken::StartArray | JsonToken::StartObject
            ));
            while depth != 0 {
                let event = reader.next_event()?.ok_or_else(invalid)?;
                match event.token {
                    JsonToken::StartArray | JsonToken::StartObject => depth += 1,
                    JsonToken::EndArray | JsonToken::EndObject => depth -= 1,
                    _ => (),
                }
                end = event.range.end;
            }
            item(&bytes[start..end])?;
        }
        if reader.next_event()?.is_some() {
            return Err(invalid());
        }
        Ok(())
    }

    pub fn null(&self, bytes: &[u8]) -> Result<bool, InputError> {
        let source = self.source(bytes)?;
        let mut reader = JsonReader::from_slice_with_control(source.bytes(), &self.control);
        Ok(reader.next_event()?.ok_or_else(invalid)?.token == JsonToken::Null)
    }

    pub fn boolean(&self, bytes: &[u8]) -> Result<bool, InputError> {
        let source = self.source(bytes)?;
        let mut reader = JsonReader::from_slice_with_control(source.bytes(), &self.control);
        let JsonToken::Bool(value) = reader.next_event()?.ok_or_else(invalid)?.token else {
            return Err(invalid());
        };
        if reader.next_event()?.is_some() {
            return Err(invalid());
        }
        Ok(value)
    }

    pub fn unsigned(&self, bytes: &[u8]) -> Result<u64, InputError> {
        let source = self.source(bytes)?;
        let mut reader = JsonReader::from_slice_with_control(source.bytes(), &self.control);
        let JsonToken::Number(text) = reader.next_event()?.ok_or_else(invalid)?.token else {
            return Err(invalid());
        };
        let value = text
            .parse::<u64>()
            .ok()
            .or_else(|| text.parse::<i64>().ok().and_then(|n| n.try_into().ok()))
            .ok_or_else(invalid)?;
        if reader.next_event()?.is_some() {
            return Err(invalid());
        }
        Ok(value)
    }

    pub fn identity(&self, bytes: &[u8]) -> Result<[u8; 16], InputError> {
        let mut value = [0; 16];
        let mut index = 0;
        self.array(bytes, |bytes| {
            let slot = value.get_mut(index).ok_or_else(invalid)?;
            *slot = self.unsigned(bytes)?.try_into().map_err(|_| invalid())?;
            index += 1;
            Ok(())
        })?;
        if index != value.len() {
            return Err(invalid());
        }
        Ok(value)
    }

    pub fn role(&self, bytes: &[u8]) -> Result<RoleIdentity, InputError> {
        let mut value = RoleIdentity::BOOTSTRAP;
        self.fields(
            bytes,
            &["oid", "object_id"],
            [3, 3],
            false,
            |field, bytes| {
                if field == 0 {
                    value.oid = self.unsigned(bytes)?.try_into().map_err(|_| invalid())?;
                } else {
                    value.object_id = self.identity(bytes)?;
                }
                Ok(())
            },
        )?;
        Ok(value)
    }

    pub fn acl<P: Copy + Default>(
        &self,
        bytes: &[u8],
        privileges: impl Fn(&Self, &[u8]) -> Result<P, InputError>,
    ) -> Result<Produced<Option<Vec<BoundAclEntry<P>>>>, InputError> {
        if self.null(bytes)? {
            return Ok(self
                .control
                .finish(None, self.control.empty_reservation())?);
        }
        let mut values = ProductionVec::new(self.control);
        self.array(bytes, |bytes| {
            let mut entry = BoundAclEntry {
                role: None,
                grantor: RoleIdentity::BOOTSTRAP,
                privileges: P::default(),
                grant_options: P::default(),
            };
            self.fields(
                bytes,
                &["role", "grantor", "privileges", "grant_options"],
                [15, 15],
                false,
                |field, bytes| {
                    match field {
                        0 if !self.null(bytes)? => entry.role = Some(self.role(bytes)?),
                        1 => entry.grantor = self.role(bytes)?,
                        2 => entry.privileges = privileges(self, bytes)?,
                        3 => entry.grant_options = privileges(self, bytes)?,
                        _ => (),
                    }
                    Ok(())
                },
            )?;
            values.push_produced(
                self.control
                    .finish(entry, self.control.empty_reservation())?,
            )?;
            Ok(())
        })?;
        let (values, memory) = values.finish()?.into_parts();
        Ok(self.control.finish(Some(values), memory)?)
    }
}
