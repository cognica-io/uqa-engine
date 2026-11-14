//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    finish, frame, iterator, Input, JsonDiagnostic, JsonPosition, JsonReadError, JsonReadWork,
    JsonValue, JsonValueKind, Parts, Projected, Read, Result,
};
use crate::ast::{AclRoleSpecification, RoleSpecification};

fn invalid() -> super::JsonInputError {
    JsonDiagnostic::message(
        "data did not match any variant of untagged enum Stored",
        JsonPosition::unlocated(),
    )
    .into()
}

async fn tagged<'a>(
    input: Input<'a>,
    content: &'static str,
    names: &'static [&'static str],
    work: &mut impl JsonReadWork<Error = JsonReadError>,
) -> Result<(usize, Option<JsonValue<'a>>)> {
    let (kind, body, numeric) = if let Some(fields) = input.value.fields() {
        let (mut kind, mut body) = (None, None);
        let fields = iterator(input.control, fields)?;
        for (name, value) in fields {
            input.checkpoint(work).await?;
            if name == "kind" {
                kind = Some(value);
            } else if name == content {
                body = Some(value);
            } else {
                return Err(invalid());
            }
        }
        (kind.ok_or_else(invalid)?, body, false)
    } else if let Some(mut values) = input.value.elements() {
        if values.len() != 2 {
            return Err(invalid());
        }
        (values.next().expect("two tag fields"), values.next(), true)
    } else {
        return Err(invalid());
    };
    let index = if let Some(name) = kind.as_str() {
        names.iter().position(|expected| *expected == name)
    } else if numeric {
        kind.number_text()
            .filter(|text| text.len() == 1)
            .and_then(|text| text.parse::<usize>().ok())
            .filter(|&index| index < names.len())
    } else {
        None
    };
    Ok((index.ok_or_else(invalid)?, body))
}

impl Projected for RoleSpecification {
    const BUFFERED: bool = true;
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            if let Some(name) = input.value.as_str() {
                return match name {
                    "CURRENT_USER" => input.scalar(Self::CurrentUser),
                    "SESSION_USER" => input.scalar(Self::SessionUser),
                    _ => {
                        let name = input.read::<String>(work).await?;
                        let mut parts = Parts::new(input.control);
                        let value = Self::Named(parts.take(name));
                        finish(parts, value)
                    }
                };
            }
            let (kind, body) = tagged(
                input,
                "name",
                &["named", "current_user", "session_user"],
                work,
            )
            .await?;
            if kind == 0 {
                let value = body
                    .filter(|value| value.as_str().is_some())
                    .ok_or_else(invalid)?;
                let name = input.child(value).read::<String>(work).await?;
                let mut parts = Parts::new(input.control);
                let value = Self::Named(parts.take(name));
                return finish(parts, value);
            }
            if body.is_some_and(|value| value.kind() != JsonValueKind::Null) {
                return Err(invalid());
            }
            input.scalar(if kind == 1 {
                Self::CurrentUser
            } else {
                Self::SessionUser
            })
        })
    }
}

impl Projected for AclRoleSpecification {
    const BUFFERED: bool = true;
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            let role = if let Some(name) = input.value.as_str() {
                if name == "PUBLIC" {
                    return input.scalar(Self::Public);
                }
                input.read::<RoleSpecification>(work).await?
            } else {
                let (kind, body) = tagged(input, "role", &["public", "role"], work).await?;
                if kind == 0 {
                    if body.is_some_and(|value| value.kind() != JsonValueKind::Null) {
                        return Err(invalid());
                    }
                    return input.scalar(Self::Public);
                }
                super::special::untagged(
                    input
                        .child(body.ok_or_else(invalid)?)
                        .read::<RoleSpecification>(work)
                        .await,
                    "Stored",
                )?
            };
            let mut parts = Parts::new(input.control);
            let value = Self::Role(parts.take(role));
            finish(parts, value)
        })
    }
}
