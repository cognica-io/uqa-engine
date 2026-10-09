//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One admitted, lazy traversal for physical JSONB output and comparisons.

use super::{
    aligned, corrupt, internal, word, Produced, ProductionControl, Result, ARRAY, COUNT_MASK,
    FALSE, HAS_OFFSET, NULL, NUMERIC, OBJECT, SCALAR, TRUE, TYPE_MASK,
};
use crate::expr::json::production::{writer, Field, Node, Values};

pub(super) enum Input<'a> {
    Bytes(&'a [u8]),
    Node(&'a Node),
}

#[derive(Clone, Copy)]
pub(super) struct Container {
    pub count: usize,
    pub object: bool,
    pub scalar: bool,
}

pub(super) enum Event<'a> {
    Begin(Container, &'static str),
    Scalar(Scalar<'a>, &'static str),
    End(Container),
}

pub(super) enum Scalar<'a> {
    Null,
    Bool(bool),
    String(Span<'a>),
    Number(Number<'a>),
}

pub(super) struct Span<'a> {
    bytes: &'a [u8],
    start: usize,
    end: usize,
}

impl<'a> Span<'a> {
    pub fn bytes(&self) -> Result<&'a [u8]> {
        self.bytes.get(self.start..self.end).ok_or_else(corrupt)
    }

    fn text(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            start: 0,
            end: text.len(),
        }
    }
}

pub(super) enum Number<'a> {
    Physical(&'a [u8], usize),
    Text(&'a str),
}

impl Number<'_> {
    pub fn text(&self, control: &ProductionControl<'_>) -> Result<Produced<String>> {
        match self {
            Self::Text(text) => Ok(control.copy_text(text)?),
            Self::Physical(bytes, start) => {
                // Numeric output follows its own varlena length, not the enclosing JEntry length.
                let bytes = bytes.get(*start..).ok_or_else(corrupt)?;
                let length = (word(bytes, 0)? >> 2) as usize;
                crate::catalog::node_tree::decode_numeric_datum_with_control(
                    bytes.get(4..length).ok_or_else(corrupt)?,
                    control,
                )
            }
        }
    }
}

pub(super) struct Stream<'a> {
    frames: Values<Frame<'a>>,
}

impl<'a> Stream<'a> {
    pub fn new(input: Input<'a>, control: &ProductionControl<'_>) -> Result<Self> {
        control.check()?;
        let mut frames = Values::new(control);
        frames.push(Frame::new(input, "")?, control)?;
        Ok(Self { frames })
    }

    pub fn next(&mut self, control: &ProductionControl<'_>) -> Result<Option<Event<'a>>> {
        loop {
            control.check()?;
            let Some(frame) = self.frames.as_mut_slice().last_mut() else {
                return Ok(None);
            };
            if !frame.started {
                frame.start(control)?;
                return Ok(Some(Event::Begin(frame.container, frame.prefix)));
            }
            match frame.next()? {
                Some((Item::Scalar(value), prefix)) => {
                    return Ok(Some(Event::Scalar(value, prefix)))
                }
                Some((Item::Container(input), prefix)) => {
                    self.frames.push(Frame::new(input, prefix)?, control)?;
                }
                None => {
                    let container = frame.container;
                    self.frames.truncate(self.frames.len() - 1);
                    return Ok(Some(Event::End(container)));
                }
            }
        }
    }
}

enum Item<'a> {
    Scalar(Scalar<'a>),
    Container(Input<'a>),
}

enum Source<'a> {
    Bytes {
        bytes: &'a [u8],
        data: usize,
        key: usize,
        value: usize,
    },
    Node {
        node: &'a Node,
        order: Option<writer::ObjectOrder<(usize, &'a Field)>>,
    },
}

struct Frame<'a> {
    source: Source<'a>,
    container: Container,
    prefix: &'static str,
    step: usize,
    started: bool,
}

impl<'a> Frame<'a> {
    fn new(input: Input<'a>, prefix: &'static str) -> Result<Self> {
        let (container, source) = match input {
            Input::Bytes(bytes) => {
                let header = word(bytes, 0)?;
                let object = match header & (ARRAY | OBJECT) {
                    ARRAY => false,
                    OBJECT => true,
                    _ => return Err(internal("unknown type of jsonb container")),
                };
                let count = (header & COUNT_MASK) as usize;
                let data = count
                    .checked_mul(if object { 8 } else { 4 })
                    .and_then(|size| size.checked_add(4))
                    .ok_or_else(corrupt)?;
                (
                    Container {
                        count,
                        object,
                        scalar: !object && header & SCALAR != 0,
                    },
                    Source::Bytes {
                        bytes,
                        data,
                        key: 0,
                        value: 0,
                    },
                )
            }
            Input::Node(node) => {
                let (count, object, scalar) = match node {
                    Node::Array(values) => (values.len(), false, false),
                    Node::Object(fields) => (fields.len(), true, false),
                    _ => (1, false, true),
                };
                (
                    Container {
                        count,
                        object,
                        scalar,
                    },
                    Source::Node { node, order: None },
                )
            }
        };
        Ok(Self {
            source,
            container,
            prefix,
            step: 0,
            started: false,
        })
    }

    fn start(&mut self, control: &ProductionControl<'_>) -> Result<()> {
        match &mut self.source {
            Source::Bytes { bytes, value, .. } if self.container.object => {
                // PostgreSQL finds the value section from the closest preceding offset entry.
                for index in (0..self.container.count).rev() {
                    control.check()?;
                    let entry = word(bytes, 4 + index * 4)?;
                    *value = value
                        .checked_add((entry & COUNT_MASK) as usize)
                        .ok_or_else(corrupt)?;
                    if entry & HAS_OFFSET != 0 {
                        break;
                    }
                }
            }
            Source::Node {
                node: Node::Object(fields),
                order,
            } => {
                *order = Some(writer::ordered(fields, true, control)?);
            }
            _ => {}
        }
        self.started = true;
        Ok(())
    }

    fn next(&mut self) -> Result<Option<(Item<'a>, &'static str)>> {
        let Container { count, object, .. } = self.container;
        if self.step == count * if object { 2 } else { 1 } {
            return Ok(None);
        }
        let key = object && self.step.is_multiple_of(2);
        let prefix = if object && !key {
            ": "
        } else if self.step != 0 {
            ", "
        } else {
            ""
        };
        let item = match &mut self.source {
            Source::Bytes {
                bytes,
                data,
                key: key_offset,
                value,
            } => {
                let (index, offset) = if key {
                    (self.step / 2, key_offset)
                } else if object {
                    (count + self.step / 2, value)
                } else {
                    (self.step, value)
                };
                let entry = word(bytes, 4 + index * 4)?;
                let length = (entry & COUNT_MASK) as usize;
                let end = if entry & HAS_OFFSET != 0 {
                    length
                } else {
                    offset.checked_add(length).ok_or_else(corrupt)?
                };
                let start = data.checked_add(*offset).ok_or_else(corrupt)?;
                let kind = entry & TYPE_MASK;
                if key && kind != 0 {
                    return Err(internal("unexpected jsonb type as object key"));
                }
                let item = match kind {
                    NULL => Item::Scalar(Scalar::Null),
                    TRUE => Item::Scalar(Scalar::Bool(true)),
                    FALSE => Item::Scalar(Scalar::Bool(false)),
                    0 => Item::Scalar(Scalar::String(Span {
                        bytes,
                        start,
                        end: data.checked_add(end).ok_or_else(corrupt)?,
                    })),
                    NUMERIC => {
                        Item::Scalar(Scalar::Number(Number::Physical(bytes, aligned(start)?)))
                    }
                    _ => Item::Container(Input::Bytes(
                        bytes.get(aligned(start)?..).ok_or_else(corrupt)?,
                    )),
                };
                *offset = end;
                item
            }
            Source::Node { node, order } => match node {
                Node::Object(_) => {
                    let field =
                        order.as_ref().expect("started object order").entries[self.step / 2].1;
                    if key {
                        Item::Scalar(Scalar::String(Span::text(&field.key)))
                    } else {
                        node_item(&field.value)
                    }
                }
                Node::Array(values) => node_item(&values[self.step]),
                node => node_item(node),
            },
        };
        self.step += 1;
        Ok(Some((item, prefix)))
    }
}

fn node_item(node: &Node) -> Item<'_> {
    match node {
        Node::Null => Item::Scalar(Scalar::Null),
        Node::Bool(value) => Item::Scalar(Scalar::Bool(*value)),
        Node::String(text) => Item::Scalar(Scalar::String(Span::text(text))),
        Node::Number(text) => Item::Scalar(Scalar::Number(Number::Text(text))),
        Node::Array(_) | Node::Object(_) => Item::Container(Input::Node(node)),
    }
}
