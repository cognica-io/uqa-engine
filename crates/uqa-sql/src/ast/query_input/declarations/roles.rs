//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{invalid, Input, InputError, JsonToken, Native, Parts, Stored, Values};
use crate::ast::{AclRoleSpecification, RoleSpecification};

struct Tagged<'a> {
    kind: uqa_core::memory::Produced<String>,
    body: Option<(&'a str, usize)>,
}

fn tagged<'a>(
    input: &mut Input<'a, '_>,
    token: JsonToken<'a>,
    content: &'static str,
) -> Result<Tagged<'a>, InputError> {
    let mut tag = None;
    let mut body = None;
    let names = ["kind", content];
    let mut cursor = input.field_cursor(token, &names, [1, 3], true)?;
    while let Some((index, token)) = cursor.next(input)? {
        if index == 0 {
            tag = Some(input.text(token)?);
        } else {
            body = Some(input.encoded(token)?);
        }
    }
    Ok(Tagged {
        kind: tag.ok_or_else(invalid)?,
        body,
    })
}

impl Stored for RoleSpecification {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let mut child = input.content_child(token)?;
        let token = child.next()?;
        let input = &mut child;
        let mut parts = Parts::new(input.control);
        if matches!(token, JsonToken::String(_)) {
            let name = input.text(token)?;
            let value = match name.as_str() {
                "CURRENT_USER" => Self::CurrentUser,
                "SESSION_USER" => Self::SessionUser,
                _ => Self::Named(parts.take(name)),
            };
            return parts.finish(value);
        }
        let Tagged { kind, body } = tagged(input, token, "name")?;
        let value = match kind.as_str() {
            "named" => {
                let (text, depth) = body.ok_or_else(invalid)?;
                let mut child = Input::content_at_depth(text, input.control, depth);
                let token = child.next()?;
                let name = parts.take(child.stored(token, values)?);
                child.end()?;
                Self::Named(name)
            }
            "current_user" | "session_user" => {
                if let Some((text, depth)) = body {
                    let mut child = Input::content_at_depth(text, input.control, depth);
                    if child.next()? != JsonToken::Null {
                        return Err(invalid());
                    }
                    child.end()?;
                }
                if kind.as_str() == "current_user" {
                    Self::CurrentUser
                } else {
                    Self::SessionUser
                }
            }
            _ => return Err(invalid()),
        };
        parts.finish(value)
    }
}

impl Stored for AclRoleSpecification {
    fn read<'a>(
        input: &mut Input<'a, '_>,
        token: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Native<Self>, InputError> {
        let mut child = input.content_child(token)?;
        let token = child.next()?;
        let input = &mut child;
        let mut parts = Parts::new(input.control);
        if matches!(token, JsonToken::String(_)) {
            let name = input.text(token)?;
            let value = match name.as_str() {
                "PUBLIC" => Self::Public,
                "CURRENT_USER" => Self::Role(RoleSpecification::CurrentUser),
                "SESSION_USER" => Self::Role(RoleSpecification::SessionUser),
                _ => Self::Role(RoleSpecification::Named(parts.take(name))),
            };
            return parts.finish(value);
        }
        let Tagged { kind, body } = tagged(input, token, "role")?;
        let value = match kind.as_str() {
            "role" => {
                let (text, depth) = body.ok_or_else(invalid)?;
                let mut child = Input::content_at_depth(text, input.control, depth);
                let token = child.next()?;
                let role = parts.take(child.stored(token, values)?);
                child.end()?;
                Self::Role(role)
            }
            "public" => {
                if let Some((text, depth)) = body {
                    let mut child = Input::content_at_depth(text, input.control, depth);
                    if child.next()? != JsonToken::Null {
                        return Err(invalid());
                    }
                    child.end()?;
                }
                Self::Public
            }
            _ => return Err(invalid()),
        };
        parts.finish(value)
    }
}
