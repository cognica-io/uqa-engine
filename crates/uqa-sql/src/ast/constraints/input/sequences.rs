//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Input, InputError, JsonToken, Parts, Produced};
use crate::ast::{
    stored_input::unit, AutoIncrement, AutoIncrementOwner, IdentitySequenceDeclaration,
    IdentitySequenceName,
};

impl<'a> Input<'a, '_> {
    pub(super) fn auto_increment(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<Option<AutoIncrement>>, InputError> {
        // The native optional untagged form validates generic content before choosing boolean or provenance syntax.
        let (text, depth) = self.encoded(token)?;
        let mut content = Input::content_at_depth(text, self.control, depth);
        let first = content.next()?;
        content.content(first)?;
        content.end()?;
        let mut input = Input::content_at_depth(text, self.control, depth);
        let token = input.next()?;
        let value = if let JsonToken::Bool(enabled) = token {
            input.control.finish(
                enabled.then(AutoIncrement::legacy),
                input.control.empty_reservation(),
            )?
        } else {
            input.maybe(token, Self::sequence_provenance)?
        };
        input.end()?;
        Ok(value)
    }

    fn sequence_provenance(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<AutoIncrement>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = AutoIncrement::legacy();
        self.fields_with_defaults(
            token,
            &["kind", "sequence", "owner", "declaration"],
            1,
            1,
            |input, index, token| {
                match index {
                    0 => value.kind = unit(&input.unit_tag(token)?)?,
                    1 => value.sequence = parts.take(input.optional_text(token)?),
                    2 => value.owner = parts.take(input.maybe(token, Self::sequence_owner)?),
                    _ => {
                        value.declaration = parts.take(input.maybe(token, |input, token| {
                            input.boxed(token, Self::identity_declaration)
                        })?);
                    }
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn sequence_owner(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<AutoIncrementOwner>, InputError> {
        let (mut table, mut column) = (None, None);
        self.fields(token, &["table", "column"], 3, |input, index, token| {
            if index == 0 {
                table = Some(input.text(token)?);
            } else {
                column = Some(input.text(token)?);
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = AutoIncrementOwner {
            table: parts.take(table.expect("required sequence owner relation")),
            column: parts.take(column.expect("required sequence owner attribute")),
        };
        parts.finish(value)
    }

    fn identity_declaration(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<IdentitySequenceDeclaration>, InputError> {
        let mut parts = Parts::new(self.control);
        let mut value = IdentitySequenceDeclaration::default();
        self.fields(
            token,
            &["name", "persistence", "sequence", "error"],
            4,
            |input, index, token| {
                match index {
                    0 => value.name = parts.take(input.maybe(token, Self::identity_sequence_name)?),
                    1 if token == JsonToken::Null => (),
                    1 => value.persistence = Some(unit(&input.unit_tag(token)?)?),
                    2 => value.sequence = parts.take(input.sequence_declaration(token)?),
                    _ => value.error = parts.take(input.maybe(token, Self::deferred_sql_error)?),
                }
                Ok(())
            },
        )?;
        parts.finish(value)
    }

    fn identity_sequence_name(
        &mut self,
        token: JsonToken<'a>,
    ) -> Result<Produced<IdentitySequenceName>, InputError> {
        let (mut schema, mut name) = (None, None);
        self.fields(token, &["schema", "name"], 2, |input, index, token| {
            if index == 0 {
                schema = Some(input.optional_text(token)?);
            } else {
                name = Some(input.text(token)?);
            }
            Ok(())
        })?;
        let mut parts = Parts::new(self.control);
        let value = IdentitySequenceName {
            schema: schema.and_then(|value| parts.take(value)),
            name: parts.take(name.expect("required sequence name")),
        };
        parts.finish(value)
    }
}
