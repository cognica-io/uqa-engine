//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered mutation targets shared by parsed and executable expressions.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The targets of one SET item. A multiple target consumes one positional subquery result.
/// Single targets retain the predecessor's string or indirection-object encoding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AssignmentTargets<E = super::Expr> {
    Single(AssignmentTarget<E>),
    Multiple(MultipleAssignmentTargets<E>),
}

/// Positional outputs remain stable when a stored MERGE loses one of its target columns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MultipleAssignmentTargets<E> {
    pub targets: Vec<AssignmentTarget<E>>,
    pub source_positions: Vec<usize>,
    pub source_width: usize,
}

impl<E> From<Vec<AssignmentTarget<E>>> for MultipleAssignmentTargets<E> {
    fn from(targets: Vec<AssignmentTarget<E>>) -> Self {
        Self {
            source_positions: (0..targets.len()).collect(),
            source_width: targets.len(),
            targets,
        }
    }
}

impl<E> From<AssignmentTarget<E>> for AssignmentTargets<E> {
    fn from(target: AssignmentTarget<E>) -> Self {
        Self::Single(target)
    }
}

impl<E> From<String> for AssignmentTargets<E> {
    fn from(column: String) -> Self {
        Self::Single(column.into())
    }
}

impl<E> From<&str> for AssignmentTargets<E> {
    fn from(column: &str) -> Self {
        column.to_owned().into()
    }
}

impl<E> AssignmentTargets<E> {
    pub fn targets(&self) -> &[AssignmentTarget<E>] {
        match self {
            Self::Single(target) => std::slice::from_ref(target),
            Self::Multiple(group) => &group.targets,
        }
    }

    pub fn targets_mut(&mut self) -> &mut [AssignmentTarget<E>] {
        match self {
            Self::Single(target) => std::slice::from_mut(target),
            Self::Multiple(group) => &mut group.targets,
        }
    }

    pub fn source_position(&self, target: usize) -> usize {
        match self {
            Self::Single(_) => target,
            Self::Multiple(group) => group.source_positions[target],
        }
    }

    pub fn column_names(&self) -> impl Iterator<Item = &str> {
        self.targets().iter().map(|target| target.column.as_str())
    }

    pub fn expressions(&self) -> impl Iterator<Item = &E> {
        self.targets()
            .iter()
            .flat_map(AssignmentTarget::expressions)
    }

    pub fn expressions_mut(&mut self) -> impl Iterator<Item = &mut E> {
        self.targets_mut()
            .iter_mut()
            .flat_map(AssignmentTarget::expressions_mut)
    }

    pub fn map<T>(self, mut map: impl FnMut(E) -> T) -> AssignmentTargets<T> {
        match self {
            Self::Single(target) => AssignmentTargets::Single(target.map(map)),
            Self::Multiple(group) => AssignmentTargets::Multiple(MultipleAssignmentTargets {
                targets: group
                    .targets
                    .into_iter()
                    .map(|target| target.map(&mut map))
                    .collect(),
                source_positions: group.source_positions,
                source_width: group.source_width,
            }),
        }
    }
}

/// Target syntax is separate from the value expression. Every bound belongs to the original input row, even when repeated targets compose writes to one column.
#[derive(Debug, Clone, PartialEq)]
pub struct AssignmentTarget<E = super::Expr> {
    pub column: String,
    pub indirection: Vec<AssignmentStep<E>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AssignmentStep<E> {
    Field(String),
    Index(Box<E>),
    Slice {
        lower: Option<Box<E>>,
        upper: Option<Box<E>>,
    },
}

impl<E> From<String> for AssignmentTarget<E> {
    fn from(column: String) -> Self {
        Self {
            column,
            indirection: Vec::new(),
        }
    }
}

impl<E> From<&str> for AssignmentTarget<E> {
    fn from(column: &str) -> Self {
        column.to_owned().into()
    }
}

impl<E> AssignmentTarget<E> {
    pub fn is_whole_column(&self) -> bool {
        self.indirection.is_empty()
    }

    pub fn expressions(&self) -> impl Iterator<Item = &E> {
        self.indirection.iter().flat_map(|step| {
            match step {
                AssignmentStep::Field(_) => [None, None],
                AssignmentStep::Index(index) => [Some(index.as_ref()), None],
                AssignmentStep::Slice { lower, upper } => [lower.as_deref(), upper.as_deref()],
            }
            .into_iter()
            .flatten()
        })
    }

    pub fn expressions_mut(&mut self) -> impl Iterator<Item = &mut E> {
        self.indirection.iter_mut().flat_map(|step| {
            match step {
                AssignmentStep::Field(_) => [None, None],
                AssignmentStep::Index(index) => [Some(index.as_mut()), None],
                AssignmentStep::Slice { lower, upper } => {
                    [lower.as_deref_mut(), upper.as_deref_mut()]
                }
            }
            .into_iter()
            .flatten()
        })
    }

    pub fn map<T>(self, mut map: impl FnMut(E) -> T) -> AssignmentTarget<T> {
        AssignmentTarget {
            column: self.column,
            indirection: self
                .indirection
                .into_iter()
                .map(|step| match step {
                    AssignmentStep::Field(field) => AssignmentStep::Field(field),
                    AssignmentStep::Index(index) => AssignmentStep::Index(Box::new(map(*index))),
                    AssignmentStep::Slice { lower, upper } => AssignmentStep::Slice {
                        lower: lower.map(|value| Box::new(map(*value))),
                        upper: upper.map(|value| Box::new(map(*value))),
                    },
                })
                .collect(),
        }
    }
}

impl<E: Serialize> Serialize for AssignmentTarget<E> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(untagged)]
        enum Target<'a, E> {
            Column(&'a str),
            Partial {
                column: &'a str,
                indirection: &'a [AssignmentStep<E>],
            },
        }
        if self.is_whole_column() {
            Target::<E>::Column(&self.column).serialize(serializer)
        } else {
            Target::Partial {
                column: &self.column,
                indirection: &self.indirection,
            }
            .serialize(serializer)
        }
    }
}

impl<'de, E: Deserialize<'de>> Deserialize<'de> for AssignmentTarget<E> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Target<E> {
            Column(String),
            Partial {
                column: String,
                indirection: Vec<AssignmentStep<E>>,
            },
        }
        Ok(match Target::deserialize(deserializer)? {
            Target::Column(column) => column.into(),
            Target::Partial {
                column,
                indirection,
            } => Self {
                column,
                indirection,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_target_encoding_preserves_predecessor_definitions() {
        let target: AssignmentTarget<i32> = serde_json::from_str("\"value\"").unwrap();
        assert!(target.is_whole_column());
        assert_eq!(target.column, "value");
        assert_eq!(serde_json::to_string(&target).unwrap(), "\"value\"");
        let group: AssignmentTargets<i32> = serde_json::from_str("\"value\"").unwrap();
        assert!(matches!(group, AssignmentTargets::Single(_)));
        assert_eq!(serde_json::to_string(&group).unwrap(), "\"value\"");
        let grouped: AssignmentTargets<i32> =
            AssignmentTargets::Multiple(vec!["a".into(), "b".into()].into());
        let encoded = serde_json::to_string(&grouped).unwrap();
        assert_eq!(
            serde_json::from_str::<AssignmentTargets<i32>>(&encoded).unwrap(),
            grouped
        );
    }

    #[test]
    fn partial_targets_keep_bound_omission_order_and_expression_rewrites() {
        let mut target = AssignmentTarget {
            column: "value".into(),
            indirection: vec![
                AssignmentStep::Index(Box::new(1)),
                AssignmentStep::Field("items".into()),
                AssignmentStep::Slice {
                    lower: None,
                    upper: Some(Box::new(2)),
                },
                AssignmentStep::Slice {
                    lower: Some(Box::new(3)),
                    upper: None,
                },
            ],
        };
        for value in target.expressions_mut() {
            *value += 10;
        }
        assert_eq!(
            target.expressions().copied().collect::<Vec<_>>(),
            [11, 12, 13]
        );
        let rendered = serde_json::to_string(&target).unwrap();
        assert_eq!(
            serde_json::from_str::<AssignmentTarget<i32>>(&rendered).unwrap(),
            target
        );
        let lowered = target.map(|value| value.to_string());
        assert_eq!(
            lowered
                .expressions()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["11", "12", "13"]
        );
        assert!(matches!(
            lowered.indirection[2],
            AssignmentStep::Slice { lower: None, .. }
        ));
        assert!(matches!(
            lowered.indirection[3],
            AssignmentStep::Slice { upper: None, .. }
        ));
    }
}
