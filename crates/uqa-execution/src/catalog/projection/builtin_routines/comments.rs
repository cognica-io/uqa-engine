//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` 18 catalog identities for object description observers.

use super::BuiltinRoutineCatalogEntry;

pub const BODY_1215: &str = r#"(({QUERY :commandType 1 :querySource 0 :canSetTag true :utilityStmt <> :resultRelation 0 :hasAggs false :hasWindowFuncs false :hasTargetSRFs false :hasSubLinks true :hasDistinctOn false :hasRecursive false :hasModifyingCTE false :hasForUpdate false :hasRowSecurity false :hasGroupRTE false :isReturn false :cteList <> :rtable ({RANGETBLENTRY :alias <> :eref {ALIAS :aliasname pg_description :colnames ("objoid" "classoid" "objsubid" "description")} :rtekind 0 :relid 2609 :inh true :relkind r :rellockmode 1 :perminfoindex 1 :tablesample <> :lateral false :inFromCl true :securityQuals <>}) :rteperminfos ({RTEPERMISSIONINFO :relid 2609 :inh true :requiredPerms 2 :checkAsUser 0 :selectedCols (b 8 9 10 11) :insertedCols (b) :updatedCols (b)}) :jointree {FROMEXPR :fromlist ({RANGETBLREF :rtindex 1}) :quals {BOOLEXPR :boolop and :args ({OPEXPR :opno 607 :opfuncid 184 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({VAR :varno 1 :varattno 1 :vartype 26 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 1 :location -1} {PARAM :paramkind 0 :paramid 1 :paramtype 26 :paramtypmod -1 :paramcollid 0 :location -1}) :location -1} {OPEXPR :opno 607 :opfuncid 184 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({VAR :varno 1 :varattno 2 :vartype 26 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 2 :location -1} {SUBLINK :subLinkType 4 :subLinkId 0 :testexpr <> :operName <> :subselect {QUERY :commandType 1 :querySource 0 :canSetTag true :utilityStmt <> :resultRelation 0 :hasAggs false :hasWindowFuncs false :hasTargetSRFs false :hasSubLinks false :hasDistinctOn false :hasRecursive false :hasModifyingCTE false :hasForUpdate false :hasRowSecurity false :hasGroupRTE false :isReturn false :cteList <> :rtable ({RANGETBLENTRY :alias <> :eref {ALIAS :aliasname pg_class :colnames ("oid" "relname" "relnamespace" "reltype" "reloftype" "relowner" "relam" "relfilenode" "reltablespace" "relpages" "reltuples" "relallvisible" "relallfrozen" "reltoastrelid" "relhasindex" "relisshared" "relpersistence" "relkind" "relnatts" "relchecks" "relhasrules" "relhastriggers" "relhassubclass" "relrowsecurity" "relforcerowsecurity" "relispopulated" "relreplident" "relispartition" "relrewrite" "relfrozenxid" "relminmxid" "relacl" "reloptions" "relpartbound")} :rtekind 0 :relid 1259 :inh true :relkind r :rellockmode 1 :perminfoindex 1 :tablesample <> :lateral false :inFromCl true :securityQuals <>}) :rteperminfos ({RTEPERMISSIONINFO :relid 1259 :inh true :requiredPerms 2 :checkAsUser 0 :selectedCols (b 8 9 10) :insertedCols (b) :updatedCols (b)}) :jointree {FROMEXPR :fromlist ({RANGETBLREF :rtindex 1}) :quals {BOOLEXPR :boolop and :args ({OPEXPR :opno 93 :opfuncid 62 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 950 :args ({VAR :varno 1 :varattno 2 :vartype 19 :vartypmod -1 :varcollid 950 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 2 :location -1} {PARAM :paramkind 0 :paramid 2 :paramtype 19 :paramtypmod -1 :paramcollid 950 :location -1}) :location -1} {OPEXPR :opno 607 :opfuncid 184 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({VAR :varno 1 :varattno 3 :vartype 26 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 3 :location -1} {RELABELTYPE :arg {CONST :consttype 4089 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true :constisnull false :location -1 :constvalue 4 [ 11 0 0 0 0 0 0 0 ]} :resulttype 26 :resulttypmod -1 :resultcollid 0 :relabelformat 2 :location -1}) :location -1}) :location -1}} :mergeActionList <> :mergeTargetRelation 0 :mergeJoinCondition <> :targetList ({TARGETENTRY :expr {VAR :varno 1 :varattno 1 :vartype 26 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 1 :location -1} :resno 1 :resname oid :ressortgroupref 0 :resorigtbl 1259 :resorigcol 1 :resjunk false}) :override 0 :onConflict <> :returningOldAlias <> :returningNewAlias <> :returningList <> :groupClause <> :groupDistinct false :groupingSets <> :havingQual <> :windowClause <> :distinctClause <> :sortClause <> :limitOffset <> :limitCount <> :limitOption 0 :rowMarks <> :setOperations <> :constraintDeps <> :withCheckOptions <> :stmt_location -1 :stmt_len -1} :location -1}) :location -1} {OPEXPR :opno 96 :opfuncid 65 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({VAR :varno 1 :varattno 3 :vartype 23 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 3 :location -1} {CONST :consttype 23 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true :constisnull false :location -1 :constvalue 4 [ 0 0 0 0 0 0 0 0 ]}) :location -1}) :location -1}} :mergeActionList <> :mergeTargetRelation 0 :mergeJoinCondition <> :targetList ({TARGETENTRY :expr {VAR :varno 1 :varattno 4 :vartype 25 :vartypmod -1 :varcollid 950 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 4 :location -1} :resno 1 :resname description :ressortgroupref 0 :resorigtbl 2609 :resorigcol 4 :resjunk false}) :override 0 :onConflict <> :returningOldAlias <> :returningNewAlias <> :returningList <> :groupClause <> :groupDistinct false :groupingSets <> :havingQual <> :windowClause <> :distinctClause <> :sortClause <> :limitOffset <> :limitCount <> :limitOption 0 :rowMarks <> :setOperations <> :constraintDeps <> :withCheckOptions <> :stmt_location -1 :stmt_len -1}))"#;

pub const BODY_1348: &str = r#"(({QUERY :commandType 1 :querySource 0 :canSetTag true :utilityStmt <> :resultRelation 0 :hasAggs false :hasWindowFuncs false :hasTargetSRFs false :hasSubLinks false :hasDistinctOn false :hasRecursive false :hasModifyingCTE false :hasForUpdate false :hasRowSecurity false :hasGroupRTE false :isReturn false :cteList <> :rtable ({RANGETBLENTRY :alias <> :eref {ALIAS :aliasname pg_description :colnames ("objoid" "classoid" "objsubid" "description")} :rtekind 0 :relid 2609 :inh true :relkind r :rellockmode 1 :perminfoindex 1 :tablesample <> :lateral false :inFromCl true :securityQuals <>}) :rteperminfos ({RTEPERMISSIONINFO :relid 2609 :inh true :requiredPerms 2 :checkAsUser 0 :selectedCols (b 8 10 11) :insertedCols (b) :updatedCols (b)}) :jointree {FROMEXPR :fromlist ({RANGETBLREF :rtindex 1}) :quals {BOOLEXPR :boolop and :args ({OPEXPR :opno 607 :opfuncid 184 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({VAR :varno 1 :varattno 1 :vartype 26 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 1 :location -1} {PARAM :paramkind 0 :paramid 1 :paramtype 26 :paramtypmod -1 :paramcollid 0 :location -1}) :location -1} {OPEXPR :opno 96 :opfuncid 65 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({VAR :varno 1 :varattno 3 :vartype 23 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 3 :location -1} {CONST :consttype 23 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true :constisnull false :location -1 :constvalue 4 [ 0 0 0 0 0 0 0 0 ]}) :location -1}) :location -1}} :mergeActionList <> :mergeTargetRelation 0 :mergeJoinCondition <> :targetList ({TARGETENTRY :expr {VAR :varno 1 :varattno 4 :vartype 25 :vartypmod -1 :varcollid 950 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 4 :location -1} :resno 1 :resname description :ressortgroupref 0 :resorigtbl 2609 :resorigcol 4 :resjunk false}) :override 0 :onConflict <> :returningOldAlias <> :returningNewAlias <> :returningList <> :groupClause <> :groupDistinct false :groupingSets <> :havingQual <> :windowClause <> :distinctClause <> :sortClause <> :limitOffset <> :limitCount <> :limitOption 0 :rowMarks <> :setOperations <> :constraintDeps <> :withCheckOptions <> :stmt_location -1 :stmt_len -1}))"#;

pub const ROUTINES: &[BuiltinRoutineCatalogEntry] = &[
    BuiltinRoutineCatalogEntry {
        oid: 1215,
        name: "obj_description",
        kind: "f",
        strict: true,
        volatility: "s",
        parallel: "s",
        leakproof: false,
        return_type: 25,
        argument_types: &[26, 19],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "",
    },
    BuiltinRoutineCatalogEntry {
        oid: 1348,
        name: "obj_description",
        kind: "f",
        strict: true,
        volatility: "s",
        parallel: "s",
        leakproof: false,
        return_type: 25,
        argument_types: &[26],
        argument_names: &[],
        default_arguments: 0,
        argument_defaults: None,
        source: "",
    },
];
