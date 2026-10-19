//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Date output styles share the physical Gregorian calendar and preserve the stored day.

use super::{TemporalDateOrder, TemporalValue};
use crate::{
    memory::{Produced, ProductionControl, ProductionString},
    ValueRetentionError,
};

/// The four date output conventions of `PostgreSQL`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TemporalDateStyle {
    #[default]
    Iso,
    Sql,
    Postgres,
    German,
}

impl TemporalValue {
    /// Format a DATE with an explicit style and field order. Other temporal kinds keep their ordinary output; canonical storage continues to use `to_sql_string_with_control`.
    pub fn to_sql_string_in_date_style_with_control(
        &self,
        style: TemporalDateStyle,
        order: TemporalDateOrder,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, ValueRetentionError> {
        control.check()?;
        if let Some(infinity) = self.infinity() {
            return control.copy_text(infinity.as_str());
        }
        match self {
            Self::Date { days } => format_date(*days, style, order, control),
            _ => self.to_sql_string_with_control(control),
        }
    }
}

pub(super) fn format_date(
    days: i32,
    style: TemporalDateStyle,
    order: TemporalDateOrder,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, ValueRetentionError> {
    let (year, month, day) = super::calendar::to_output_calendar(days);
    let displayed = if year > 0 { year } else { 1 - year };
    let mut out = ProductionString::new(*control);
    let rendered = match style {
        TemporalDateStyle::Iso => {
            control.format(format_args!("{displayed:04}-{month:02}-{day:02}"))?
        }
        TemporalDateStyle::German => {
            control.format(format_args!("{day:02}.{month:02}.{displayed:04}"))?
        }
        TemporalDateStyle::Sql | TemporalDateStyle::Postgres => {
            let (first, second) = if order == TemporalDateOrder::DayMonthYear {
                (day, month)
            } else {
                (month, day)
            };
            let separator = if style == TemporalDateStyle::Sql {
                '/'
            } else {
                '-'
            };
            control.format(format_args!(
                "{first:02}{separator}{second:02}{separator}{displayed:04}"
            ))?
        }
    };
    out.push_str(&rendered)?;
    drop(rendered);
    if year <= 0 {
        out.push_str(" BC")?;
    }
    out.finish()
}

#[cfg(test)]
mod tests;
