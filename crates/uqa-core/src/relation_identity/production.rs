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

#[cfg(test)]
mod input_tests;
mod work;
use work::{inline, Work};

impl RelationReference<'_> {
    /// Decode borrowed components directly into their retained identity buffers.
    pub fn identity_with_control(
        &self,
        default_schema: RelationName<'_>,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<super::RelationIdentity>, ValueRetentionError> {
        inline(identity(*self, default_schema, control, &mut ()))
    }

    /// Decode the same original components through retained work, admitting native schema/name buffers under the supplied production allowance.
    pub async fn identity_from_input(
        &self,
        default_schema: RelationName<'_>,
        control: &ProductionControl<'_>,
        input: &mut crate::image_io::AsyncImageInput<'_>,
    ) -> Result<Produced<super::RelationIdentity>, ValueRetentionError> {
        identity(*self, default_schema, control, input).await
    }

    pub fn render_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        inline(render(*self, control, &mut ()))
    }

    /// Render the same canonical spelling through a retained input's work channel without reading source bytes.
    pub async fn render_from(
        &self,
        input: &mut crate::image_io::AsyncImageInput<'_>,
    ) -> Result<crate::memory::Budgeted<String>, ValueRetentionError> {
        let control = ProductionControl::new(input.memory, input.cancellation, input.cancellation);
        Ok(render(*self, &control, input)
            .await?
            .into_budgeted()
            .expect("native relation rendering retains its allowance"))
    }
}

async fn identity(
    reference: RelationReference<'_>,
    default_schema: RelationName<'_>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<super::RelationIdentity>, ValueRetentionError> {
    control.check()?;
    work.checkpoint().await?;
    control.check()?;
    let schema =
        decoded_component(reference.schema.unwrap_or(default_schema), control, work).await?;
    let name = decoded_component(reference.name, control, work).await?;
    let (schema, schema_memory) = schema.into_parts();
    let (name, name_memory) = name.into_parts();
    control.finish(
        super::RelationIdentity { schema, name },
        control.combine(schema_memory, name_memory),
    )
}

async fn decoded_component(
    name: RelationName<'_>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<String>, ValueRetentionError> {
    let mut output = ProductionString::new(*control);
    for (index, character) in name.chars().enumerate() {
        if index % 1024 == 0 {
            control.check()?;
            work.checkpoint().await?;
            control.check()?;
        }
        output.push(character)?;
    }
    output.finish()
}

async fn render(
    reference: RelationReference<'_>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<Produced<String>, ValueRetentionError> {
    control.check()?;
    let mut output = ProductionString::new(*control);
    if let Some(schema) = reference.schema {
        component(schema, &mut output, control, work).await?;
        output.push('.')?;
    }
    component(reference.name, &mut output, control, work).await?;
    output.finish()
}

async fn component(
    name: RelationName<'_>,
    output: &mut ProductionString<'_>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> Result<(), ValueRetentionError> {
    let mut bare = name.chars().next().is_some();
    for (index, character) in name.chars().enumerate() {
        if index % 1024 == 0 {
            work.checkpoint().await?;
        }
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
    for (index, character) in name.chars().enumerate() {
        if index % 1024 == 0 {
            work.checkpoint().await?;
        }
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
