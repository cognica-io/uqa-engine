//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow existing diagnostic payloads and admit only the containers and text this renderer constructs.

use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::SQLError;

pub(super) enum Property<'a> {
    Text(&'a str),
    OwnedText(Produced<String>),
    Unsigned(u64),
    Float(f64),
    Object(Produced<Vec<(&'a str, Self)>>),
    Array(Produced<Vec<Self>>),
    Json(&'a serde_json::Value),
    Lines(&'a str),
}

pub(super) struct Object<'a, 'c> {
    fields: ProductionVec<'c, (&'a str, Property<'a>)>,
    control: ProductionControl<'c>,
}

impl<'a, 'c> Object<'a, 'c> {
    pub(super) fn new(control: ProductionControl<'c>) -> Self {
        Self {
            fields: ProductionVec::new(control),
            control,
        }
    }

    pub(super) fn insert(&mut self, name: &'a str, value: Property<'a>) -> Result<(), SQLError> {
        // Every nested property owns its own lease; this reservation covers only this object's vector.
        self.fields.push_produced(
            self.control
                .finish((name, value), self.control.empty_reservation())?,
        )?;
        Ok(())
    }

    pub(super) fn finish(self) -> Result<Property<'a>, SQLError> {
        Ok(Property::Object(self.fields.finish()?))
    }
}

pub(super) fn object<'a>(
    control: ProductionControl<'_>,
    fields: impl IntoIterator<Item = (&'a str, Property<'a>)>,
) -> Result<Property<'a>, SQLError> {
    let mut output = Object::new(control);
    for (key, value) in fields {
        output.insert(key, value)?;
    }
    output.finish()
}

pub(super) fn array<'a>(
    control: ProductionControl<'_>,
    values: impl IntoIterator<Item = Result<Property<'a>, SQLError>>,
) -> Result<Property<'a>, SQLError> {
    let mut output = ProductionVec::new(control);
    for value in values {
        output.push_produced(control.finish(value?, control.empty_reservation())?)?;
    }
    Ok(Property::Array(output.finish()?))
}

#[derive(Clone, Copy)]
pub(super) enum View<'a> {
    Native(&'a Property<'a>),
    Json(&'a serde_json::Value),
    Text(&'a str),
}

pub(super) enum Kind<'a> {
    Null,
    Bool(bool),
    Text(&'a str),
    Number(serde_json::Number),
    Object(Fields<'a>),
    Array(Elements<'a>),
}

impl<'a> View<'a> {
    pub(super) fn kind(self) -> Kind<'a> {
        match self {
            Self::Text(text) => Kind::Text(text),
            Self::Native(value) => match value {
                Property::Text(text) => Kind::Text(text),
                Property::OwnedText(text) => Kind::Text(text),
                Property::Unsigned(value) => Kind::Number((*value).into()),
                Property::Float(value) => {
                    serde_json::Number::from_f64(*value).map_or(Kind::Null, Kind::Number)
                }
                Property::Object(fields) => Kind::Object(Fields::Native(fields.iter())),
                Property::Array(values) => Kind::Array(Elements::Native(values.iter())),
                Property::Json(value) => Self::Json(value).kind(),
                Property::Lines(text) => Kind::Array(Elements::Lines(text.lines())),
            },
            Self::Json(value) => match value {
                serde_json::Value::Null => Kind::Null,
                serde_json::Value::Bool(value) => Kind::Bool(*value),
                serde_json::Value::Number(value) => Kind::Number(value.clone()),
                serde_json::Value::String(value) => Kind::Text(value),
                serde_json::Value::Object(fields) => Kind::Object(Fields::Json(fields.iter())),
                serde_json::Value::Array(values) => Kind::Array(Elements::Json(values.iter())),
            },
        }
    }
}

pub(super) enum Fields<'a> {
    Native(std::slice::Iter<'a, (&'a str, Property<'a>)>),
    Json(serde_json::map::Iter<'a>),
}

impl<'a> Iterator for Fields<'a> {
    type Item = (&'a str, View<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Native(fields) => fields
                .next()
                .map(|(key, value)| (*key, View::Native(value))),
            Self::Json(fields) => fields
                .next()
                .map(|(key, value)| (key.as_str(), View::Json(value))),
        }
    }
}

pub(super) enum Elements<'a> {
    Native(std::slice::Iter<'a, Property<'a>>),
    Json(std::slice::Iter<'a, serde_json::Value>),
    Lines(std::str::Lines<'a>),
}

impl<'a> Iterator for Elements<'a> {
    type Item = View<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Native(values) => values.next().map(View::Native),
            Self::Json(values) => values.next().map(View::Json),
            Self::Lines(values) => values.next().map(View::Text),
        }
    }
}
