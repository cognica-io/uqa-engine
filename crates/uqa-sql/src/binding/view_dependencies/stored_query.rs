//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical view catalogs store lowered query JSON. Project just function resolution fields while consuming the original grammar. Literal values, evaluated rows and unrelated declarations never become call sites.

use super::StoredFunctionSite;
use crate::SQLError;
use uqa_core::{
    json::{decode_json_string_with_work, JsonReadError, JsonReadWork, JsonReader, JsonToken},
    memory::{BudgetedVec, Produced, ProductionControl},
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    ExpressionPlan,
    Query,
    Relational,
    Block,
    SetOperation,
    Values,
    SourceValues,
    Cast,
    Cte,
    Cycle,
    Source,
    Join,
    Subquery,
    FunctionGroup,
    Scalar,
    Binary,
    Operand,
    Between,
    InList,
    Case,
    CompositeRow,
    ExpressionField,
    WindowDefinition,
    Window,
    Frame,
    FrameBound,
    Call(StoredFunctionSite),
    Binding,
    Selected,
    Builtin,
    Name,
    List(&'static Self),
    Ignored,
}

impl Role {
    fn field(self, key: &str) -> Self {
        use Role::{
            Between, Binary, Binding, Block, Builtin, Call, Case, CompositeRow, Cte, Cycle,
            ExpressionField, Frame, FrameBound, FunctionGroup, Ignored, InList, Join, List, Name,
            Operand, Query, Relational, Scalar, Selected, SetOperation, Source, Subquery, Values,
            Window, WindowDefinition,
        };
        match (self, key) {
            (Self::ExpressionPlan, "scalar") => Scalar,
            (Self::ExpressionPlan, "subqueries") => List(&Query),
            (Query, "root") => Relational,
            (Query, "ctes") => List(&Cte),
            (Cte, "query") => Query,
            (Cte, "cycle") => Cycle,
            (SetOperation, "left" | "right") | (Subquery, "body") => Query,
            (Relational, "QueryBlock") => Block,
            (Relational, "SetOp") => SetOperation,
            (Relational, "Values") => Values,
            (Source, "Values") => Self::SourceValues,
            (Block, "from") | (Join, "left" | "right") => Source,
            (Block, "projections" | "order_by") | (SetOperation | Call(_) | Window, "order_by") => {
                List(&ExpressionField)
            }
            (Block, "group_by" | "distinct_on")
            | (Window, "partition_by")
            | (Call(_), "args")
            | (InList, "list")
            | (CompositeRow, "items") => List(&Scalar),
            (Block, "grouping_sets") | (Values | Self::SourceValues, "rows") | (Case, "when") => {
                List(&List(&Scalar))
            }
            (Block, "where" | "having" | "limit" | "offset")
            | (SetOperation, "limit" | "offset")
            | (Cycle, "mark_value" | "mark_default")
            | (Join, "on")
            | (ExpressionField | Operand | InList | Self::Cast, "expr")
            | (Between, "expr" | "low" | "high")
            | (Binary, "lhs" | "rhs")
            | (Case, "base" | "else_branch")
            | (Call(_), "filter")
            | (FrameBound, "Preceding" | "Following") => Scalar,
            (Block, "windows") => List(&WindowDefinition),
            (Block | SetOperation | Values, "subqueries") => List(&Query),
            (Source, "Join") => Join,
            (Source, "Subquery") => Subquery,
            (Source, "Function") => Call(StoredFunctionSite::From),
            (Source, "FunctionGroup") => FunctionGroup,
            (FunctionGroup, "functions") => List(&Call(StoredFunctionSite::From)),
            (Scalar, "Func") => Call(StoredFunctionSite::Expression),
            (Scalar, "WindowCall") => Call(StoredFunctionSite::Window),
            (Scalar, "Array" | "Row" | "And" | "Or") => List(&Scalar),
            (Scalar, "UnaryMinus" | "Not") => Scalar,
            (Scalar, "IsNull" | "Collated" | "InSubquery") => Operand,
            (Scalar, "Cast") => Self::Cast,
            (Scalar, "Binary") => Binary,
            (Scalar, "Between") => Between,
            (Scalar, "InList") => InList,
            (Scalar, "Case") => Case,
            (Scalar, "CompositeRow") => CompositeRow,
            (Call(_), "name") => Name,
            (Call(_), "binding") => Binding,
            (Call(_) | WindowDefinition, "spec") => Window,
            (Window, "frame") => Frame,
            (Frame, "start" | "end") => FrameBound,
            (
                Binding,
                "object_id" | "dispatch" | "resolution_error" | "composite_field" | "assignment",
            ) => Selected,
            (Binding, "builtin") => Builtin,
            _ => Ignored,
        }
    }
}

// Serde accepts positional and named fields for native structs. Both encodings use the same field roles, and lists retain their element role.
impl Role {
    fn element(self, index: usize) -> Self {
        if let Self::List(element) = self {
            return *element;
        }
        let fields: &[&str] = match self {
            Self::ExpressionPlan => &["scalar", "subqueries"],
            Self::Query => &["", "ctes", "root"],
            Self::Cycle => &["", "", "mark_value", "mark_default"],
            Self::Block => &[
                "windows",
                "projections",
                "",
                "from",
                "where",
                "",
                "group_by",
                "grouping_sets",
                "",
                "having",
                "order_by",
                "limit",
                "",
                "offset",
                "",
                "distinct_on",
                "subqueries",
            ],
            Self::SetOperation => &[
                "",
                "",
                "left",
                "right",
                "order_by",
                "limit",
                "",
                "offset",
                "subqueries",
            ],
            Self::Values => &["rows", "subqueries"],
            Self::SourceValues => &["", "rows"],
            Self::Join => &["", "left", "right", "", "on"],
            Self::Subquery => &["", "body"],
            Self::FunctionGroup => &["functions"],
            Self::Binary => &["", "lhs", "rhs"],
            Self::Operand | Self::ExpressionField => &["expr"],
            Self::Cast => &["", "expr"],
            Self::Between => &["expr", "low", "high"],
            Self::InList => &["expr", "list"],
            Self::Case => &["base", "when", "else_branch"],
            Self::CompositeRow => &["items"],
            Self::WindowDefinition => &["", "", "spec"],
            Self::Window => &["", "partition_by", "order_by", "frame"],
            Self::Frame => &["", "start", "end"],
            Self::Call(StoredFunctionSite::Expression) => {
                &["name", "binding", "args", "", "order_by", "", "filter"]
            }
            Self::Call(StoredFunctionSite::Window) => &["name", "args", "spec", "filter"],
            Self::Call(StoredFunctionSite::From) => &["name", "binding", "", "", "args"],
            // Binding and CTE both flatten a tagged object and require named fields.
            _ => &[],
        };
        fields
            .get(index)
            .map_or(Self::Ignored, |name| self.field(name))
    }
}

struct Frame {
    role: Role,
    field: Role,
    index: Option<usize>,
    name: Option<Produced<String>>,
    selected: bool,
}

impl Frame {
    fn new(role: Role, array: bool) -> Self {
        Self {
            role,
            field: Role::Ignored,
            index: array.then_some(0),
            name: None,
            selected: false,
        }
    }
}

fn json_error(error: JsonReadError) -> SQLError {
    match error {
        JsonReadError::Memory(error) => error.into(),
        JsonReadError::Cancelled(error) => error.into(),
        _ => SQLError::Internal("invalid stored query representation".into()),
    }
}

/// Observe runtime function lookups in the existing durable lowered-query representation. The physical reader and SQL scratch share the caller's byte/poll and allocation controls. Object field order is immaterial. No query, application callback or SQL literal is evaluated.
pub async fn visit_stored_query_json_runtime_calls<W: JsonReadWork<Error = JsonReadError>>(
    bytes: &[u8],
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> Result<(), SQLError> {
    visit_json_runtime_calls(bytes, Role::Query, control, work, visit).await
}

pub(crate) async fn visit_expression_plan_json_runtime_calls<
    W: JsonReadWork<Error = JsonReadError>,
>(
    bytes: &[u8],
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> Result<(), SQLError> {
    visit_json_runtime_calls(bytes, Role::ExpressionPlan, control, work, visit).await
}

async fn visit_json_runtime_calls<W: JsonReadWork<Error = JsonReadError>>(
    bytes: &[u8],
    root: Role,
    control: &ProductionControl<'_>,
    work: &mut W,
    visit: &mut (impl FnMut(&str, StoredFunctionSite) -> Result<(), SQLError> + Send),
) -> Result<(), SQLError> {
    let mut input = JsonReader::from_slice_with_control(bytes, control);
    let mut frames = BudgetedVec::<Frame>::new(control.budget().expect("admitted stored query"));
    let mut ignored = 0usize;
    while let Some(event) = input.next_event_with_work(work).await.map_err(json_error)? {
        work.checkpoint().await.map_err(json_error)?;
        if ignored != 0 {
            match event.token {
                JsonToken::StartArray | JsonToken::StartObject => ignored += 1,
                JsonToken::EndArray | JsonToken::EndObject => ignored -= 1,
                _ => (),
            }
            continue;
        }
        match event.token {
            JsonToken::Key(key) => {
                let frame = frames.last_mut().expect("JSON object");
                let key = decode_json_string_with_work(key, control, work)
                    .await
                    .map_err(json_error)?;
                frame.field = frame.role.field(&key);
            }
            JsonToken::EndArray | JsonToken::EndObject => {
                let frame = frames.pop().expect("JSON container");
                if frame.role == Role::Binding && frame.selected {
                    frames.last_mut().expect("call owning binding").selected = true;
                }
                if let Role::Call(site) = frame.role {
                    if !frame.selected {
                        let name = frame
                            .name
                            .ok_or_else(|| json_error(JsonReadError::InvalidJson))?;
                        visit(&name, site)?;
                    }
                }
            }
            token => {
                let parent = frames.last_mut();
                let field = parent.as_deref().map_or(root, |frame| {
                    frame
                        .index
                        .map_or(frame.field, |index| frame.role.element(index))
                });
                if let Some(frame) = parent {
                    if let Some(index) = &mut frame.index {
                        *index += 1;
                    }
                    if field == Role::Selected && !matches!(token, JsonToken::Null)
                        || field == Role::Builtin && matches!(token, JsonToken::Bool(true))
                    {
                        frame.selected = true;
                    }
                    if field == Role::Name {
                        if let JsonToken::String(name) = token {
                            frame.name = Some(
                                decode_json_string_with_work(name, control, work)
                                    .await
                                    .map_err(json_error)?,
                            );
                        }
                    }
                }
                if matches!(token, JsonToken::StartArray | JsonToken::StartObject) {
                    match field {
                        Role::Ignored | Role::Selected => ignored = 1,
                        _ => frames
                            .push(Frame::new(field, matches!(token, JsonToken::StartArray)))?,
                    }
                }
            }
        }
    }
    Ok(())
}
