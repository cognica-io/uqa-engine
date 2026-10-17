/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 *
 * utils/palloc.h
 *	  PostgreSQL's allocation interface for the imported regular expression
 *	  engine.
 *
 * The engine allocates only through regex/regcustom.h, which passes
 * MCXT_ALLOC_NO_OOM.  The implementation in uqa_pg_regex.c charges every
 * chunk to the resource hooks of the current engine operation and keeps it in
 * that operation's memory context.  As in PostgreSQL, requests above
 * MaxAllocSize without MCXT_ALLOC_HUGE and overflowing array sizes raise an
 * error, and a failed request returns NULL only with MCXT_ALLOC_NO_OOM.
 */
#ifndef UQA_PG_REGEX_PALLOC_H
#define UQA_PG_REGEX_PALLOC_H

#define MCXT_ALLOC_HUGE			0x01	/* allow huge allocation (> 1 GB) */
#define MCXT_ALLOC_NO_OOM		0x02	/* no failure if out-of-memory */
#define MCXT_ALLOC_ZERO			0x04	/* zero allocated memory */

#define MaxAllocSize	((Size) 0x3fffffff) /* 1 gigabyte - 1 */
#define MaxAllocHugeSize	(SIZE_MAX / 2)

extern void *palloc_extended(Size size, int flags);
extern void *repalloc_extended(void *pointer, Size size, int flags);
extern void *palloc_mul_extended(Size s1, Size s2, int flags);
extern void *repalloc_mul_extended(void *p, Size s1, Size s2, int flags);
extern void pfree(void *pointer);

#define palloc_array_extended(type, count, flags) \
	((type *) palloc_mul_extended(sizeof(type), count, flags))
#define repalloc_array_extended(pointer, type, count, flags) \
	((type *) repalloc_mul_extended(pointer, sizeof(type), count, flags))

#endif							/* UQA_PG_REGEX_PALLOC_H */
