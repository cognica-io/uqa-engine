//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain fields retain native scalar defaults and checks without catalog binding or execution.

use super::{
    boolean, number, Input, InputError, JsonToken, Parts, Produced, RelationIdentity, RoleIdentity,
};
use crate::{
    ast::{
        stored_input::{optional, Values},
        ColumnType, CreateDomain, DomainCheck, DomainNotNull, Expr,
    },
    catalog::domain::StoredDomain,
    SQLError,
};
use uqa_core::{memory::MemoryBudget, CancellationToken};

impl StoredDomain {
    /// Decode an existing stored scalar domain declaration with admitted fields and native expression payloads. Complete identity, authority, claim and constraint validation remains with the catalog owner.
    pub fn from_stored_schema_json(
        text: &str,
        memory: &MemoryBudget,
        original: &CancellationToken,
        invoking: &CancellationToken,
    ) -> Result<Produced<Self>, SQLError> {
        let values = Values {
            memory,
            original,
            invoking,
        };
        let mut input = Input::new(text, values.control());
        let first = input.next()?;
        let value = input.stored_domain(first, &values)?;
        input.end()?;
        Ok(value)
    }
}

impl<'a> Input<'a, '_> {
    fn stored_domain(
        &mut self,
        first: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<StoredDomain>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = StoredDomain {
            object_id: [0; 16],
            oid: 0,
            array_oid: None,
            identity: RelationIdentity::new("", ""),
            owner: RoleIdentity::BOOTSTRAP,
            definition: empty_definition(),
            array_name: None,
            usage_acl: None,
        };
        self.fields_with_defaults(
            first,
            &[
                "object_id",
                "oid",
                "array_oid",
                "identity",
                "owner",
                "definition",
                "array_name",
                "usage_acl",
            ],
            59,
            59,
            |input, index, token| {
                match index {
                    0 => value.object_id = input.identity(token)?,
                    1 => value.oid = number(token)?,
                    2 => value.array_oid = optional(token)?,
                    3 => value.identity = parts.take(input.stored_relation(token)?),
                    4 => value.owner = input.stored_role_identity(token)?,
                    5 => value.definition = parts.take(input.domain_declaration(token, values)?),
                    6 => value.array_name = parts.take(input.optional_text(token)?),
                    7 => value.usage_acl = parts.take(input.stored_usage_acl(token)?),
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn domain_declaration(
        &mut self,
        first: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<CreateDomain>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = empty_definition();
        self.fields_with_defaults(
            first,
            &[
                "name",
                "base",
                "collation",
                "collation_oid",
                "default",
                "not_null",
                "checks",
            ],
            67,
            119,
            |input, index, token| {
                match index {
                    0 => value.name = parts.take(input.text(token)?),
                    1 => value.base = parts.take(input.column(token)?),
                    2 => value.collation = parts.take(input.optional_text(token)?),
                    3 => value.collation_oid = optional(token)?,
                    4 => {
                        value.default =
                            parts.take(input.maybe(token, |input, token| {
                                input.schema_expression(token, values)
                            })?);
                    }
                    5 => value.not_null = parts.take(input.maybe(token, Self::domain_not_null)?),
                    6 => {
                        value.checks = parts.take(
                            input.array(token, |input, token| input.domain_check(token, values))?,
                        );
                    }
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn domain_not_null(
        &mut self,
        first: JsonToken<'a>,
    ) -> Result<Produced<DomainNotNull>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = DomainNotNull {
            name: None,
            catalog_identity: None,
        };
        self.fields_with_defaults(
            first,
            &["name", "catalog_identity"],
            0,
            1,
            |input, index, token| {
                match index {
                    0 => value.name = parts.take(input.optional_text(token)?),
                    1 => value.catalog_identity = input.optional_catalog_identity(token)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn domain_check(
        &mut self,
        first: JsonToken<'a>,
        values: &Values<'_>,
    ) -> Result<Produced<DomainCheck>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = DomainCheck {
            name: None,
            catalog_identity: None,
            expression: Expr::Star,
            validated: true,
        };
        self.fields_with_defaults(
            first,
            &["name", "catalog_identity", "expression", "validated"],
            4,
            5,
            |input, index, token| {
                match index {
                    0 => value.name = parts.take(input.optional_text(token)?),
                    1 => value.catalog_identity = input.optional_catalog_identity(token)?,
                    2 => value.expression = parts.take(input.schema_expression(token, values)?),
                    3 => value.validated = boolean(token)?,
                    _ => unreachable!(),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }
}

fn empty_definition() -> CreateDomain {
    CreateDomain {
        name: String::new(),
        base: ColumnType::Integer,
        collation: None,
        collation_oid: None,
        default: None,
        not_null: None,
        checks: Vec::new(),
    }
}
