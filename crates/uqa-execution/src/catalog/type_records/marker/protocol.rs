//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Observe the installed public Serde representations without naming dependency-private fields.

use serde::{
    ser::{Impossible, SerializeStruct},
    Serialize, Serializer,
};
use uqa_core::memory::ProductionControl;
use uqa_storage::StorageBackendResult;

#[derive(Clone, Copy)]
pub(crate) struct Keys {
    pub number: &'static str,
    pub raw: &'static str,
}

pub(crate) fn keys(control: &ProductionControl<'_>) -> StorageBackendResult<Keys> {
    control.check()?;
    // The fixed scalar probe owns only its small numeric spelling; no source-dependent allocation occurs here.
    let _scratch = control.reserve(128)?;
    Ok(Keys {
        number: serde_json::Number::from(1u8).serialize(KeySerializer)?,
        raw: serde_json::value::RawValue::NULL.serialize(KeySerializer)?,
    })
}

fn unsupported<T>() -> Result<T, serde_json::Error> {
    Err(serde::ser::Error::custom(
        "unexpected generic JSON scalar representation",
    ))
}
struct KeySerializer;
struct Field(Option<&'static str>);
impl SerializeStruct for Field {
    type Ok = &'static str;
    type Error = serde_json::Error;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        _: &T,
    ) -> Result<(), Self::Error> {
        if self.0.replace(key).is_some() {
            return unsupported();
        }
        Ok(())
    }
    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.0
            .ok_or_else(|| serde::ser::Error::custom("missing scalar field"))
    }
}
macro_rules! primitive {
    ($($method:ident($ty:ty)),* $(,)?) => {$ (
        fn $method(self, _: $ty) -> Result<Self::Ok, Self::Error> { unsupported() }
    )*};
}
impl Serializer for KeySerializer {
    type Ok = &'static str;
    type Error = serde_json::Error;
    type SerializeSeq = Impossible<Self::Ok, Self::Error>;
    type SerializeTuple = Impossible<Self::Ok, Self::Error>;
    type SerializeTupleStruct = Impossible<Self::Ok, Self::Error>;
    type SerializeTupleVariant = Impossible<Self::Ok, Self::Error>;
    type SerializeMap = Impossible<Self::Ok, Self::Error>;
    type SerializeStruct = Field;
    type SerializeStructVariant = Impossible<Self::Ok, Self::Error>;
    primitive!(
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_f32(f32),
        serialize_f64(f64),
        serialize_char(char),
        serialize_str(&str),
        serialize_bytes(&[u8])
    );
    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_some<T: ?Sized + Serialize>(self, _: &T) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: &T,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<Self::Ok, Self::Error> {
        unsupported()
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        unsupported()
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Self::Error> {
        unsupported()
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        unsupported()
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        unsupported()
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        unsupported()
    }
    fn serialize_struct(
        self,
        _: &'static str,
        length: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        if length != 1 {
            return unsupported();
        }
        Ok(Field(None))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        unsupported()
    }
}
