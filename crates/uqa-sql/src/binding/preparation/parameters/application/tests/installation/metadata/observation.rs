//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

pub(super) trait Plan: MetadataTarget {
    fn as_plan_ref(&self) -> PlanRef<'_>;
    fn into_replacement(self) -> QueryReplacement;
}

impl Plan for MutableExecutablePlan {
    fn as_plan_ref(&self) -> PlanRef<'_> {
        MutableExecutablePlan::as_plan_ref(self)
    }
    fn into_replacement(self) -> QueryReplacement {
        let Self::Query(query) = self else {
            panic!("query replacement");
        };
        query.into()
    }
}

impl Plan for RetainedExecutablePlan {
    fn as_plan_ref(&self) -> PlanRef<'_> {
        RetainedExecutablePlan::as_plan_ref(self)
    }
    fn into_replacement(self) -> QueryReplacement {
        self.try_into().unwrap()
    }
}

type QueryObserver<'a> = dyn FnMut(&QueryPlan) -> Result<QueryMetadataUpdate, SQLError> + 'a;

struct Queries<'a, 'b>(&'a mut QueryObserver<'b>);

impl MetadataObserver<()> for Queries<'_, '_> {
    fn visit_table_queries(&self) -> bool {
        true
    }
    fn query(
        &mut self,
        query: &QueryPlan,
        _: &ProductionControl<'_>,
        (): &mut (),
    ) -> impl std::future::Future<Output = Result<Option<QueryMetadataUpdate>, SQLError>> + Send
    {
        std::future::ready((self.0)(query).map(Some))
    }
}

pub(super) fn observe_queries<P: MetadataTarget>(
    plan: &mut P,
    control: &ProductionControl<'_>,
    observe: &mut QueryObserver<'_>,
) -> Result<(), SQLError> {
    inline(plan.apply_metadata_from(&mut Queries(observe), control, &mut ()))
}
