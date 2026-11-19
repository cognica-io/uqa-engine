//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{frame, iterator, Input, JsonReadError, JsonReadWork, Projected, Read, Result};
use crate::ast::RoleAttribute;
use std::cmp::Ordering;
use uqa_core::memory::{Budgeted, MapKeyInput, OwnedMap, OwnedSet};

impl<T: Projected + Copy + Ord> Projected for OwnedSet<T> {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            input.checkpoint(work).await?;
            let Some(values) = input.value.elements() else {
                return Err(input.type_error("an ordered set", work).await?);
            };
            let mut output = Self::new();
            for value in values {
                let value = input.child(value).read::<T>(work).await?.into_parts().0;
                output
                    .insert_budgeted(value, input.control.budget().expect("admitted set input"))?;
            }
            input.scalar(output)
        })
    }
}

impl<T: Projected> Projected for OwnedMap<String, T> {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            input.checkpoint(work).await?;
            let Some(fields) = input.value.fields() else {
                return Err(input.type_error("an ordered map", work).await?);
            };
            let fields = iterator(input.control, fields)?;
            let mut output = Self::new();
            for (name, value) in fields {
                let key = input.text(name, work).await?;
                let value = input.child(value).read::<T>(work).await?;
                let (key, key_memory) = key.into_parts();
                let (value, value_memory) = value.into_parts();
                let memory = input
                    .control
                    .combine(key_memory, value_memory)
                    .expect("admitted map input");
                let replaced = output
                    .insert_admitted_key_from(
                        Budgeted::new(key, memory),
                        value,
                        &mut Comparison { input, work },
                    )
                    .await?;
                debug_assert!(replaced.is_none(), "generic map names are unique");
            }
            input.scalar(output)
        })
    }
}

struct Comparison<'a, 'w, W> {
    input: Input<'a>,
    work: &'w mut W,
}
impl<W: JsonReadWork<Error = JsonReadError>> MapKeyInput<String> for Comparison<'_, '_, W> {
    type Error = super::JsonInputError;
    async fn checkpoint(&mut self) -> Result<()> {
        self.input.checkpoint(self.work).await
    }
    async fn compare(&mut self, query: &String, stored: &String) -> Result<Ordering> {
        for (left, right) in query
            .as_bytes()
            .chunks(4096)
            .zip(stored.as_bytes().chunks(4096))
        {
            self.checkpoint().await?;
            let ordering = left.cmp(right);
            if ordering != Ordering::Equal {
                return Ok(ordering);
            }
        }
        Ok(query.len().cmp(&stored.len()))
    }
}

impl Projected for OwnedMap<RoleAttribute, bool> {
    fn read_value<'i, 'a>(
        input: Input<'i>,
        work: &'a mut crate::ast::query_input::projected::Work<'i>,
    ) -> Result<Read<'a, Self>> {
        frame(input.control, async move {
            input.checkpoint(work).await?;
            let Some(fields) = input.value.fields() else {
                return Err(input.type_error("an ordered map", work).await?);
            };
            let fields = iterator(input.control, fields)?;
            let mut output = Self::new();
            for (name, value) in fields {
                input.checkpoint(work).await?;
                let key = match name {
                    "Superuser" => RoleAttribute::Superuser,
                    "Inherit" => RoleAttribute::Inherit,
                    "CreateRole" => RoleAttribute::CreateRole,
                    "CreateDb" => RoleAttribute::CreateDb,
                    "Login" => RoleAttribute::Login,
                    "Replication" => RoleAttribute::Replication,
                    "BypassRls" => RoleAttribute::BypassRls,
                    _ => {
                        return Err(input
                            .unknown_variant(
                                name,
                                &[
                                    "Superuser",
                                    "Inherit",
                                    "CreateRole",
                                    "CreateDb",
                                    "Login",
                                    "Replication",
                                    "BypassRls",
                                ],
                                work,
                            )
                            .await?)
                    }
                };
                let value = input.child(value).read::<bool>(work).await?.into_parts().0;
                output.insert_budgeted(
                    key,
                    value,
                    input.control.budget().expect("admitted attribute input"),
                )?;
            }
            input.scalar(output)
        })
    }
}
