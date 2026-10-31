//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    copy, optional_text, Layout, ProductionControl, ProductionVec, RowSchema, SQLError,
    SchemaBuildMetadata, SchemaMemory, SchemaSet, ScoreSource,
};

impl RowSchema {
    /// Replace positional wildcard visibility without changing addressable columns or physical slots.
    pub fn with_wildcard_hidden_positions_with_control(
        input: &Self,
        positions: impl IntoIterator<Item = usize>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut metadata = copy::metadata(input, control)?;
        metadata.wildcard_hidden = SchemaSet::from_produced(
            positions
                .into_iter()
                .map(|position| Ok(control.finish(position, control.empty_reservation())?)),
            control,
        )?;
        copy::with_metadata(input, metadata, control)
    }

    /// Replace the public relation boundary while preserving physical identities and retained descriptors.
    pub fn with_relation_qualifier_with_control(
        input: &Self,
        qualifier: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut layout = Layout::new(control);
        layout.append(input, 0, Some(qualifier), control)?;
        let mut scores = ProductionVec::new(*control);
        for source in &input.index.cold.score_sources {
            let (qualifier, memory) = optional_text(Some(qualifier), control)?.into_parts();
            scores.push_produced(control.finish(
                ScoreSource {
                    qualifier,
                    column: source.column,
                },
                memory,
            )?)?;
        }
        let (score_sources, memory) = scores.finish()?.into_parts();
        let source = copy::metadata(input, control)?;
        let metadata = SchemaBuildMetadata {
            collations: source.collations,
            row_types: input
                .index
                .cold
                .row_types
                .requalified_with_control(input, qualifier, control)?,
            record_fields: source.record_fields,
            lateral_restrictions: Self::requalified_column_names_with_control(input, control)?,
            internal: source.internal,
            internal_types: source.internal_types,
            score_sources,
            open_qualifiers: SchemaSet::from_produced(
                input
                    .columns_are_open(None)
                    .then_some(Some(qualifier))
                    .into_iter()
                    .map(|name| optional_text(name, control)),
                control,
            )?,
            wildcard_hidden: source.wildcard_hidden,
            memory: SchemaMemory::new(memory),
            ..SchemaBuildMetadata::empty_with_control(control)
        };
        layout.finish(input.physical_width(), metadata, control)
    }
}

impl RowSchema {
    pub fn with_open_columns_with_control(
        input: &Self,
        qualifier: Option<&str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut metadata = copy::metadata(input, control)?;
        metadata.open_qualifiers = SchemaSet::from_produced(
            input
                .index
                .cold
                .open_qualifiers
                .iter()
                .map(|name| name.as_deref())
                .chain(std::iter::once(qualifier))
                .map(|name| optional_text(name, control)),
            control,
        )?;
        metadata.local_width = None;
        metadata.lateral_restrictions = None;
        copy::with_metadata(input, metadata, control)
    }
}
