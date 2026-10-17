/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 *
 * uqa_pg_regex.c
 *	  Engine operations, error frames and PostgreSQL backend services for the
 *	  imported regular expression engine.
 *
 * PostgreSQL's engine leaves through ereport(ERROR) and longjmp when a query
 * is canceled or an allocation request is invalid, and it relies on the
 * error handler to reset the memory context of the failed operation.  This
 * file keeps that structure: each operation function establishes an error
 * frame with sigsetjmp before it enters the engine, and the failure paths
 * below siglongjmp to the innermost frame.  The same path abandons an
 * operation whose stack cannot go deeper, which the Rust wrapper then runs
 * again on a larger stack.  The frame belongs to C code called
 * directly by Rust, and the Rust callbacks return before any longjmp starts,
 * so no Rust frame lies between a longjmp and its frame.  Frames form a
 * per-thread stack, so a resource hook may itself run another operation, and
 * operations on different threads never share state.
 */

/*
 * sigsetjmp and siglongjmp are POSIX interfaces, which C library headers hide
 * in strict ISO C modes unless POSIX is requested.
 */
#if !defined(_WIN32) && !defined(_POSIX_C_SOURCE)
#define _POSIX_C_SOURCE 200809L
#endif

#include "postgres.h"

#include <setjmp.h>

#include "miscadmin.h"
#include "uqa_pg_regex.h"

#if defined(_MSC_VER)
#define UQA_PG_REGEX_THREAD_LOCAL __declspec(thread)
#else
#define UQA_PG_REGEX_THREAD_LOCAL _Thread_local
#endif

/*
 * Non-local jumps as PostgreSQL's c.h selects them: sigsetjmp without saving
 * the signal mask, which on BSD-derived systems would otherwise cost a system
 * call on every operation; plain setjmp on Windows, which has no sigsetjmp;
 * and the compiler builtins on MinGW-w64, whose setjmp support is unreliable.
 */
#if defined(_WIN32) && defined(__MINGW64__)
typedef intptr_t uqa_pg_regex_jmp_buf[5];
#define uqa_pg_regex_sigsetjmp(env) __builtin_setjmp(env)
#define uqa_pg_regex_siglongjmp(env) __builtin_longjmp(env, 1)
#elif defined(_WIN32)
typedef jmp_buf uqa_pg_regex_jmp_buf;
#define uqa_pg_regex_sigsetjmp(env) setjmp(env)
#define uqa_pg_regex_siglongjmp(env) longjmp(env, 1)
#else
typedef sigjmp_buf uqa_pg_regex_jmp_buf;
#define uqa_pg_regex_sigsetjmp(env) sigsetjmp(env, 0)
#define uqa_pg_regex_siglongjmp(env) siglongjmp(env, 1)
#endif

typedef struct UQAPgRegexFrame
{
	uqa_pg_regex_jmp_buf env;	/* error frame of the operation */
	void	   *operation;		/* Rust operation serving the callbacks */
	UQAPgRegexThrown *thrown;	/* failure report, owned by the caller */
	uintptr_t	stack_base;		/* stack position at operation entry */
	Oid			locale;			/* token set by pg_set_regex_collation */
	struct UQAPgRegexFrame *previous;	/* enclosing operation, if any */
} UQAPgRegexFrame;

static UQA_PG_REGEX_THREAD_LOCAL UQAPgRegexFrame *current_frame = NULL;

/*
 * The engine's static functions are reachable only through the operation
 * functions below, so a missing frame is an internal defect.  It cannot be
 * reported through a frame, and returning would let the engine continue
 * without its memory context, so the process stops as PostgreSQL stops on a
 * failed assertion.
 */
pg_noreturn static void
missing_frame(void)
{
	fputs("uqa-pg-regex: engine service called outside an engine operation\n",
		  stderr);
	abort();
}

static UQAPgRegexFrame *
require_frame(void)
{
	UQAPgRegexFrame *frame = current_frame;

	if (frame == NULL)
		missing_frame();
	return frame;
}

static void
enter_frame(UQAPgRegexFrame *frame, void *operation,
			UQAPgRegexThrown *thrown)
{
	frame->operation = operation;
	frame->thrown = thrown;
	frame->stack_base = (uintptr_t) frame;
	frame->locale = InvalidOid;
	frame->previous = current_frame;
	thrown->kind = UQA_PG_REGEX_THROW_NONE;
	thrown->first = 0;
	thrown->second = 0;
	current_frame = frame;
}

static void
leave_frame(UQAPgRegexFrame *frame)
{
	current_frame = frame->previous;
}

/* Record the failure and leave the engine through the innermost frame. */
pg_noreturn static void
throw_failure(int kind, size_t first, size_t second)
{
	UQAPgRegexFrame *frame = require_frame();

	frame->thrown->kind = kind;
	frame->thrown->first = first;
	frame->thrown->second = second;
	uqa_pg_regex_siglongjmp(frame->env);
}

static void
check_callback(int status)
{
	if (status == UQA_PG_REGEX_CALLBACK_PANICKED)
		throw_failure(UQA_PG_REGEX_THROW_CALLBACK, 0, 0);
}

/*
 * uqa_pg_regex_compile - compile a pattern into a new regex_t
 *
 * The regex_t itself is allocated in the operation's memory context, which
 * the Rust wrapper keeps as the compiled expression's context.
 */
int
uqa_pg_regex_compile(void *operation, const pg_wchar *pattern, size_t length,
					 int cflags, Oid locale, regex_t **result,
					 UQAPgRegexThrown *thrown)
{
	UQAPgRegexFrame frame;
	regex_t    *re;
	int			code;

	*result = NULL;
	enter_frame(&frame, operation, thrown);
	if (uqa_pg_regex_sigsetjmp(frame.env) != 0)
	{
		leave_frame(&frame);
		return UQA_PG_REGEX_THROWN;
	}
	re = (regex_t *) palloc_extended(sizeof(regex_t), MCXT_ALLOC_NO_OOM);
	if (re == NULL)
		code = REG_ESPACE;
	else
	{
		/* on failure, pg_regcomp leaves nothing for pg_regfree */
		code = pg_regcomp(re, pattern, length, cflags, locale);
		if (code == REG_OKAY)
			*result = re;
		else
			pfree(re);
	}
	leave_frame(&frame);
	return code;
}

/*
 * uqa_pg_regex_execute - run pg_regexec without execution flags or details
 */
int
uqa_pg_regex_execute(void *operation, regex_t *re, const pg_wchar *data,
					 size_t length, size_t search_start, size_t nmatch,
					 regmatch_t *pmatch, UQAPgRegexThrown *thrown)
{
	UQAPgRegexFrame frame;
	int			code;

	enter_frame(&frame, operation, thrown);
	if (uqa_pg_regex_sigsetjmp(frame.env) != 0)
	{
		leave_frame(&frame);
		return UQA_PG_REGEX_THROWN;
	}
	code = pg_regexec(re, data, length, search_start, NULL, nmatch, pmatch, 0);
	leave_frame(&frame);
	return code;
}

size_t
uqa_pg_regex_subexpressions(const regex_t *re)
{
	return re->re_nsub;
}

/* The explanation of an error code, exactly as pg_regerror writes it. */
size_t
uqa_pg_regex_error_message(int code, char *buffer, size_t size)
{
	return pg_regerror(code, NULL, buffer, size);
}

/* Layout check for the Rust mirror of regmatch_t. */
size_t
uqa_pg_regex_match_size(void)
{
	return sizeof(regmatch_t);
}

/*
 * PostgreSQL's allocation functions, as the engine uses them.
 *
 * Size validation follows PostgreSQL's memory-context code: requests above
 * MaxAllocSize without MCXT_ALLOC_HUGE raise "invalid memory alloc request
 * size", and an overflowing array size raises "invalid memory allocation
 * request size".  A request the resource hooks refuse, or that the allocator
 * cannot satisfy, returns NULL with MCXT_ALLOC_NO_OOM and raises "out of
 * memory" otherwise.
 */
static void
check_request_size(Size size, int flags)
{
	if (size > MaxAllocSize &&
		((flags & MCXT_ALLOC_HUGE) == 0 || size > MaxAllocHugeSize))
		throw_failure(UQA_PG_REGEX_THROW_ALLOC_SIZE, size, 0);
}

static Size
request_product(Size s1, Size s2)
{
	if (s1 != 0 && s2 > SIZE_MAX / s1)
		throw_failure(UQA_PG_REGEX_THROW_ALLOC_PRODUCT, s1, s2);
	return s1 * s2;
}

static void *
allocation_result(void *result, Size size, int flags)
{
	if (result == NULL && (flags & MCXT_ALLOC_NO_OOM) == 0)
		throw_failure(UQA_PG_REGEX_THROW_OUT_OF_MEMORY, size, 0);
	return result;
}

void *
palloc_extended(Size size, int flags)
{
	UQAPgRegexFrame *frame = require_frame();
	void	   *result = NULL;

	check_request_size(size, flags);
	check_callback(uqa_pg_regex_callback_allocate(frame->operation, size,
												  &result));
	if (result != NULL && (flags & MCXT_ALLOC_ZERO) != 0)
		memset(result, 0, size);
	return allocation_result(result, size, flags);
}

void *
repalloc_extended(void *pointer, Size size, int flags)
{
	UQAPgRegexFrame *frame = require_frame();
	void	   *result = NULL;

	check_request_size(size, flags);
	check_callback(uqa_pg_regex_callback_reallocate(frame->operation, pointer,
													size, &result));
	return allocation_result(result, size, flags);
}

void *
palloc_mul_extended(Size s1, Size s2, int flags)
{
	return palloc_extended(request_product(s1, s2), flags);
}

void *
repalloc_mul_extended(void *p, Size s1, Size s2, int flags)
{
	return repalloc_extended(p, request_product(s1, s2), flags);
}

/* Like the free() of Spencer's original allocation macros, NULL is ignored. */
void
pfree(void *pointer)
{
	UQAPgRegexFrame *frame = require_frame();

	if (pointer == NULL)
		return;
	check_callback(uqa_pg_regex_callback_free(frame->operation, pointer));
}

/* CHECK_FOR_INTERRUPTS(): stop the operation when the caller cancels it. */
void
uqa_pg_regex_check_for_interrupts(void)
{
	UQAPgRegexFrame *frame = require_frame();
	int			status = uqa_pg_regex_callback_interrupt(frame->operation);

	check_callback(status);
	if (status == UQA_PG_REGEX_CALLBACK_REFUSED)
		throw_failure(UQA_PG_REGEX_THROW_CANCELED, 0, 0);
}

/*
 * stack_is_too_deep(): measure the stack used since the operation entered the
 * engine and let the Rust wrapper decide.  A refusal makes the engine fail
 * with REG_ETOOBIG; a stack that cannot go deeper with room to spare abandons
 * the operation, which the wrapper runs again on a larger stack.
 */
bool
stack_is_too_deep(void)
{
	UQAPgRegexFrame *frame = require_frame();
	char		marker;
	uintptr_t	here = (uintptr_t) &marker;
	size_t		depth;
	int			status;

	depth = here < frame->stack_base ? frame->stack_base - here :
		here - frame->stack_base;
	status = uqa_pg_regex_callback_stack(frame->operation, depth);
	check_callback(status);
	if (status == UQA_PG_REGEX_CALLBACK_RESTART)
		throw_failure(UQA_PG_REGEX_THROW_STACK, depth, 0);
	return status == UQA_PG_REGEX_CALLBACK_REFUSED;
}

/*
 * pg_char_and_wchar_strncmp(): compare a char string with a pg_wchar string,
 * as PostgreSQL's wstrncmp.c does for the engine's name lookups.
 */
int
pg_char_and_wchar_strncmp(const char *s1, const pg_wchar *s2, size_t n)
{
	for (; n > 0; n--, s1++, s2++)
	{
		pg_wchar	c1 = (pg_wchar) (unsigned char) *s1;

		if (c1 != *s2)
			return (int) (c1 - *s2);
		if (c1 == 0)
			break;
	}
	return 0;
}

/* Locale state for regc_pg_locale.c. */
void
uqa_pg_regex_select_locale(Oid locale)
{
	UQAPgRegexFrame *frame = require_frame();

	switch (locale)
	{
		case UQA_PG_REGEX_LOCALE_C:
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
		case UQA_PG_REGEX_LOCALE_ICU:
			frame->locale = locale;
			return;
	}
	throw_failure(UQA_PG_REGEX_THROW_LOCALE, locale, 0);
}

Oid
uqa_pg_regex_current_locale(void)
{
	return require_frame()->locale;
}

/*
 * The character functions of regc_pg_locale.c run only after
 * uqa_pg_regex_select_locale() has accepted a token, and they can run below
 * the Rust character-class cache, where no longjmp may start.  An unknown
 * token there is therefore an internal defect that stops the process.
 */
void
uqa_pg_regex_invalid_locale_state(void)
{
	fputs("uqa-pg-regex: character function called without a selected locale\n",
		  stderr);
	abort();
}
