//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical names stream decoded components into their admitted destination.

use super::{RelationName, RelationReference};
use crate::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

impl RelationReference<'_> {
    /// Decode borrowed components directly into their retained identity buffers.
    pub fn identity_with_control(
        &self,
        default_schema: RelationName<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<super::RelationIdentity>, ValueRetentionError> {
        let schema = control.format(format_args!("{}", self.schema.unwrap_or(default_schema)))?;
        let name = control.format(format_args!("{}", self.name))?;
        let (schema, schema_memory) = schema.into_parts();
        let (name, name_memory) = name.into_parts();
        control.finish(
            super::RelationIdentity { schema, name },
            control.combine(schema_memory, name_memory),
        )
    }

    pub fn render_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        control.check()?;
        let mut output = ProductionString::new(*control);
        if let Some(schema) = self.schema {
            component(schema, &mut output, control)?;
            output.push('.')?;
        }
        component(self.name, &mut output, control)?;
        output.finish()
    }
}

fn component(
    name: RelationName<'_>,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
) -> Result<(), ValueRetentionError> {
    let mut bare = name.chars().next().is_some();
    for (index, character) in name.chars().enumerate() {
        control.check()?;
        bare &= match character {
            'a'..='z' | '_' => true,
            '0'..='9' | '$' => index != 0,
            _ => false,
        };
    }
    if !bare {
        output.push('"')?;
    }
    for character in name.chars() {
        control.check()?;
        if character == '"' {
            output.push('"')?;
        }
        output.push(character)?;
    }
    if !bare {
        output.push('"')?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{memory::MemoryBudget, CancellationToken, RelationIdentity};

    #[test]
    fn reference_names_admit_exact_storage_and_preserve_escaped_components() {
        for (source, expected) in [
            ("public.table", "public.table"),
            ("\"A.Schema\".\"a\"\"b\"", "\"A.Schema\".\"a\"\"b\""),
            ("\"한국어\"", "\"한국어\""),
            ("\"foo\".\"bar\"", "foo.bar"),
        ] {
            let reference = RelationIdentity::borrowed_reference(source).unwrap();
            let budget = MemoryBudget::new(1024);
            let cancellation = CancellationToken::new();
            let control = ProductionControl::new(&budget, &cancellation, &cancellation);
            let mut output = None;
            let measured = allocation_counter::measure(|| {
                output = Some(reference.render_with_control(&control).unwrap());
            });
            let output = output.unwrap();
            assert_eq!(output.as_str(), expected);
            assert_eq!(measured.bytes_current, budget.used() as i64);
            assert_eq!(budget.used(), output.capacity());
            drop(output);
            assert_eq!(budget.used(), 0);
            for available in 0..expected.len() {
                let budget = MemoryBudget::new(available);
                let control = ProductionControl::new(&budget, &cancellation, &cancellation);
                assert!(matches!(
                    reference.render_with_control(&control),
                    Err(ValueRetentionError::Memory(_))
                ));
                assert_eq!(budget.used(), 0);
            }
        }
    }
}
