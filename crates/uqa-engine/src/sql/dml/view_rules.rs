//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub(super) use uqa_execution::mutation::rules::views::prepare_view_rule_batches;
pub(super) type ViewRuleBatchRequest<'a> =
    uqa_execution::mutation::rules::views::ViewRuleBatchRequest<
        'a,
        crate::session::StatementReadSnapshot,
    >;
