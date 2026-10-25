//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow existing diagnostic payloads and admit only the containers and text this renderer constructs.

use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::{result::explain::ExplainValue, SQLError};

pub(super) enum Property<'a> {
    Text(&'a str),
    OwnedText(Produced<String>),
    Unsigned(u64),
    Float(f64),
    Object(Produced<Vec<(&'a str, Self)>>),
    Array(Produced<Vec<Self>>),
    Captured(&'a ExplainValue),
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
    Captured(&'a ExplainValue),
    Text(&'a str),
}

pub(super) enum Kind<'a> {
    Null,
    Bool(bool),
    Text(&'a str),
    Number(Number),
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
                Property::Unsigned(value) => Kind::Number(Number::Unsigned(*value)),
                Property::Float(value) => {
                    if value.is_finite() {
                        Kind::Number(Number::Float(*value))
                    } else {
                        Kind::Null
                    }
                }
                Property::Object(fields) => Kind::Object(Fields::Native(fields.iter())),
                Property::Array(values) => Kind::Array(Elements::Native(values.iter())),
                Property::Captured(value) => Self::Captured(value).kind(),
                Property::Lines(text) => Kind::Array(Elements::Lines(text.lines())),
            },
            Self::Captured(value) => match value {
                ExplainValue::Null => Kind::Null,
                ExplainValue::Bool(value) => Kind::Bool(*value),
                ExplainValue::Unsigned(value) => Kind::Number(Number::Unsigned(*value)),
                ExplainValue::Float(value) => {
                    if value.is_finite() {
                        Kind::Number(Number::Float(*value))
                    } else {
                        Kind::Null
                    }
                }
                ExplainValue::Label(value) => Kind::Text(value),
                ExplainValue::Text(value) => Kind::Text(value),
                ExplainValue::Object(fields) => Kind::Object(Fields::Captured(fields.iter())),
            },
        }
    }
}

pub(super) enum Fields<'a> {
    Native(std::slice::Iter<'a, (&'a str, Property<'a>)>),
    Captured(std::slice::Iter<'a, (&'static str, ExplainValue)>),
}

impl<'a> Iterator for Fields<'a> {
    type Item = (&'a str, View<'a>);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Native(fields) => fields
                .next()
                .map(|(key, value)| (*key, View::Native(value))),
            Self::Captured(fields) => fields
                .next()
                .map(|(key, value)| (*key, View::Captured(value))),
        }
    }
}

pub(super) enum Elements<'a> {
    Native(std::slice::Iter<'a, Property<'a>>),
    Lines(std::str::Lines<'a>),
}

impl<'a> Iterator for Elements<'a> {
    type Item = View<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Native(values) => values.next().map(View::Native),
            Self::Lines(values) => values.next().map(View::Text),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Number {
    Unsigned(u64),
    Float(f64),
}

impl std::fmt::Display for Number {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsigned(value) => write!(output, "{value}"),
            Self::Float(value) => write!(output, "{value:?}"),
        }
    }
}
