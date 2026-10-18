/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 *
 * miscadmin.h
 *	  PostgreSQL's interrupt and stack-depth checks for the imported regular
 *	  expression engine.
 *
 * regex/regcustom.h defines INTERRUPT() as CHECK_FOR_INTERRUPTS(), and
 * regcomp.c answers STACK_TOO_DEEP() with stack_is_too_deep().  Both consult
 * the resource hooks of the current engine operation.  A cancellation leaves
 * the engine through the operation's C error frame, as PostgreSQL's query
 * cancel leaves it through ereport(ERROR).
 */
#ifndef UQA_PG_REGEX_MISCADMIN_H
#define UQA_PG_REGEX_MISCADMIN_H

extern void uqa_pg_regex_check_for_interrupts(void);
extern bool stack_is_too_deep(void);

#define CHECK_FOR_INTERRUPTS() uqa_pg_regex_check_for_interrupts()

#endif							/* UQA_PG_REGEX_MISCADMIN_H */
