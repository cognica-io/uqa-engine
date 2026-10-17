/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 *
 * uqa_pg_regex.h
 *	  The interface between the imported PostgreSQL regular expression engine,
 *	  the UQA C shim and the Rust wrapper of uqa-pg-regex.
 *
 * Every engine call enters through one of the operation functions below.
 * Each establishes a per-thread error frame and records the Rust operation
 * whose memory context and resource hooks serve the call.  Cancellation,
 * panics of the Rust hooks and PostgreSQL's allocation-size errors leave the
 * engine with longjmp to that frame, which lies in C code called directly by
 * Rust; no longjmp crosses a Rust frame.  The Rust wrapper then frees every
 * chunk of the abandoned operation.
 */
#ifndef UQA_PG_REGEX_H
#define UQA_PG_REGEX_H

#include "postgres.h"

#include "regex/regex.h"

/*
 * Locale tokens.  The engine passes its collation argument unchanged to
 * pg_set_regex_collation(); the Rust wrapper resolves collations with
 * PostgreSQL's checks and passes one of these tokens instead of an OID.
 */
#define UQA_PG_REGEX_LOCALE_C				1	/* C and POSIX ctype */
#define UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8	2	/* builtin, casemap_full off */
#define UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE 3	/* builtin, casemap_full on */
#define UQA_PG_REGEX_LOCALE_ICU				4	/* ICU provider */

/* Result of an operation that left the engine through its error frame. */
#define UQA_PG_REGEX_THROWN (-1000)

/* Why an operation left the engine through its error frame. */
#define UQA_PG_REGEX_THROW_NONE				0
#define UQA_PG_REGEX_THROW_CANCELED			1	/* cancellation hook */
#define UQA_PG_REGEX_THROW_CALLBACK			2	/* a Rust hook panicked */
#define UQA_PG_REGEX_THROW_ALLOC_SIZE		3	/* request above MaxAllocSize */
#define UQA_PG_REGEX_THROW_ALLOC_PRODUCT	4	/* overflowing array size */
#define UQA_PG_REGEX_THROW_OUT_OF_MEMORY	5	/* failure without NO_OOM */
#define UQA_PG_REGEX_THROW_LOCALE			6	/* invalid locale token */

typedef struct UQAPgRegexThrown
{
	int			kind;			/* UQA_PG_REGEX_THROW_* */
	size_t		first;			/* request size or first factor */
	size_t		second;			/* second factor */
} UQAPgRegexThrown;

/* Results of the Rust callbacks. */
#define UQA_PG_REGEX_CALLBACK_OK		0
#define UQA_PG_REGEX_CALLBACK_REFUSED	1	/* allocation refused or failed, cancel requested, stack too deep */
#define UQA_PG_REGEX_CALLBACK_PANICKED	2	/* the hook panicked; the payload is kept */

/* Engine operations, called by the Rust wrapper. */
extern int	uqa_pg_regex_compile(void *operation, const pg_wchar *pattern,
								 size_t length, int cflags, Oid locale,
								 regex_t **result, UQAPgRegexThrown *thrown);
extern int	uqa_pg_regex_execute(void *operation, regex_t *re,
								 const pg_wchar *data, size_t length,
								 size_t search_start, size_t nmatch,
								 regmatch_t *pmatch, UQAPgRegexThrown *thrown);
extern int	uqa_pg_regex_release(void *operation, regex_t *re,
								 UQAPgRegexThrown *thrown);
extern size_t uqa_pg_regex_subexpressions(const regex_t *re);
extern size_t uqa_pg_regex_error_message(int code, char *buffer, size_t size);
extern size_t uqa_pg_regex_match_size(void);

/* Rust callbacks for the current operation. */
extern int	uqa_pg_regex_callback_allocate(void *operation, size_t size,
										   void **result);
extern int	uqa_pg_regex_callback_reallocate(void *operation, void *pointer,
											 size_t size, void **result);
extern int	uqa_pg_regex_callback_free(void *operation, void *pointer);
extern int	uqa_pg_regex_callback_interrupt(void *operation);
extern int	uqa_pg_regex_callback_stack(void *operation, size_t depth);

/* Locale state of the current operation, used by regc_pg_locale.c. */
extern void uqa_pg_regex_select_locale(Oid locale);
extern Oid	uqa_pg_regex_current_locale(void);
pg_noreturn extern void uqa_pg_regex_invalid_locale_state(void);

/*
 * ICU4C character functions, implemented by the Rust wrapper over ICU4X data
 * of the same Unicode version.
 */
extern int	uqa_pg_regex_icu_isdigit(pg_wchar c);
extern int	uqa_pg_regex_icu_isalpha(pg_wchar c);
extern int	uqa_pg_regex_icu_isalnum(pg_wchar c);
extern int	uqa_pg_regex_icu_isupper(pg_wchar c);
extern int	uqa_pg_regex_icu_islower(pg_wchar c);
extern int	uqa_pg_regex_icu_isgraph(pg_wchar c);
extern int	uqa_pg_regex_icu_isprint(pg_wchar c);
extern int	uqa_pg_regex_icu_ispunct(pg_wchar c);
extern int	uqa_pg_regex_icu_isspace(pg_wchar c);
extern pg_wchar uqa_pg_regex_icu_toupper(pg_wchar c);
extern pg_wchar uqa_pg_regex_icu_tolower(pg_wchar c);

/*
 * Process-wide character-class member lists, kept by the Rust wrapper.  The
 * lists are immutable once built and are never freed.
 */
typedef int (*uqa_pg_regex_probe) (pg_wchar c);

extern void uqa_pg_regex_class_members(Oid locale, int cclasscode,
									   uqa_pg_regex_probe probe,
									   pg_wchar max_chr,
									   const pg_wchar **chrs, int *nchrs,
									   const pg_wchar **ranges, int *nranges);

#endif							/* UQA_PG_REGEX_H */
