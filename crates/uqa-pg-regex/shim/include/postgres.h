/*
 * Unified Query Algebra
 *
 * Copyright (c) 2023-2026 Cognica, Inc.
 *
 * postgres.h
 *	  The PostgreSQL backend definitions used by the imported regular
 *	  expression engine and Unicode tables.
 *
 * Every imported translation unit includes this file first, directly or
 * through regex/regcustom.h.  It supplies the c.h-level types and macros the
 * imported files use, PostgreSQL's memory allocation interface, and the
 * renaming of every global symbol those files define.
 */
#ifndef UQA_PG_REGEX_POSTGRES_H
#define UQA_PG_REGEX_POSTGRES_H

/*
 * Global symbols.  The imported files keep PostgreSQL's names in source; the
 * linker sees them with a uqa_pg_regex_ prefix so that they cannot resolve to,
 * or collide with, the PostgreSQL parser objects that uqa-pg-query links into
 * the same program.  These macros precede every declaration and definition.
 */
#define palloc_extended uqa_pg_regex_palloc_extended
#define repalloc_extended uqa_pg_regex_repalloc_extended
#define palloc_mul_extended uqa_pg_regex_palloc_mul_extended
#define repalloc_mul_extended uqa_pg_regex_repalloc_mul_extended
#define pfree uqa_pg_regex_pfree
#define stack_is_too_deep uqa_pg_regex_stack_is_too_deep
#define pg_char_and_wchar_strncmp uqa_pg_regex_char_and_wchar_strncmp
#define pg_regcomp uqa_pg_regex_regcomp
#define pg_regexec uqa_pg_regex_regexec
#define pg_regfree uqa_pg_regex_regfree
#define pg_regerror uqa_pg_regex_regerror
#define pg_set_regex_collation uqa_pg_regex_set_regex_collation
#define pg_reg_getcolor uqa_pg_regex_reg_getcolor
#define unicode_category uqa_pg_regex_unicode_category
#define unicode_category_string uqa_pg_regex_unicode_category_string
#define unicode_category_abbrev uqa_pg_regex_unicode_category_abbrev
#define pg_u_prop_alphabetic uqa_pg_regex_u_prop_alphabetic
#define pg_u_prop_lowercase uqa_pg_regex_u_prop_lowercase
#define pg_u_prop_uppercase uqa_pg_regex_u_prop_uppercase
#define pg_u_prop_cased uqa_pg_regex_u_prop_cased
#define pg_u_prop_case_ignorable uqa_pg_regex_u_prop_case_ignorable
#define pg_u_prop_white_space uqa_pg_regex_u_prop_white_space
#define pg_u_prop_hex_digit uqa_pg_regex_u_prop_hex_digit
#define pg_u_prop_join_control uqa_pg_regex_u_prop_join_control
#define pg_u_isdigit uqa_pg_regex_u_isdigit
#define pg_u_isalpha uqa_pg_regex_u_isalpha
#define pg_u_isalnum uqa_pg_regex_u_isalnum
#define pg_u_isword uqa_pg_regex_u_isword
#define pg_u_isupper uqa_pg_regex_u_isupper
#define pg_u_islower uqa_pg_regex_u_islower
#define pg_u_isblank uqa_pg_regex_u_isblank
#define pg_u_iscntrl uqa_pg_regex_u_iscntrl
#define pg_u_isgraph uqa_pg_regex_u_isgraph
#define pg_u_isprint uqa_pg_regex_u_isprint
#define pg_u_ispunct uqa_pg_regex_u_ispunct
#define pg_u_isspace uqa_pg_regex_u_isspace
#define pg_u_isxdigit uqa_pg_regex_u_isxdigit
#define unicode_lowercase_simple uqa_pg_regex_unicode_lowercase_simple
#define unicode_titlecase_simple uqa_pg_regex_unicode_titlecase_simple
#define unicode_uppercase_simple uqa_pg_regex_unicode_uppercase_simple
#define unicode_casefold_simple uqa_pg_regex_unicode_casefold_simple
#define unicode_strlower uqa_pg_regex_unicode_strlower
#define unicode_strtitle uqa_pg_regex_unicode_strtitle
#define unicode_strupper uqa_pg_regex_unicode_strupper
#define unicode_strfold uqa_pg_regex_unicode_strfold

#include <limits.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#ifndef _MSC_VER
#include <sys/types.h>
#endif

/* Compiler characteristics, as c.h derives them. */
#if defined(_MSC_VER) && !defined(__cplusplus) && !defined(__STDC_VERSION__)
/* MSVC spells the C99 inline function specifier __inline before C11 mode. */
#define inline __inline
#endif

#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#define pg_noreturn _Noreturn
#elif defined(__GNUC__)
#define pg_noreturn __attribute__((noreturn))
#elif defined(_MSC_VER)
#define pg_noreturn __declspec(noreturn)
#else
#define pg_noreturn
#endif

#if defined(__GNUC__)
#define likely(x) __builtin_expect((x) != 0, 1)
#define unlikely(x) __builtin_expect((x) != 0, 0)
#else
#define likely(x) ((x) != 0)
#define unlikely(x) ((x) != 0)
#endif

#define PGDLLIMPORT
#define FLEXIBLE_ARRAY_MEMBER
#define lengthof(array) (sizeof (array) / sizeof ((array)[0]))

/*
 * Assertions are compiled out, as in production PostgreSQL builds where
 * USE_ASSERT_CHECKING is not defined.  UQA reproduces that behavior, which is
 * also what an independent PostgreSQL 18.4 server observes.
 */
#define Assert(condition) ((void) true)

/* Fixed-width integer and catalog types. */
typedef int8_t int8;
typedef int16_t int16;
typedef int32_t int32;
typedef int64_t int64;
typedef uint8_t uint8;
typedef uint16_t uint16;
typedef uint32_t uint32;
typedef uint64_t uint64;
typedef size_t Size;
#ifdef _MSC_VER
typedef ptrdiff_t ssize_t;
#endif

#ifdef WIN32
/*
 * Compilers that define WIN32, such as MinGW, expose pg_wchar.h's
 * declaration of PostgreSQL's Windows UTF-16 message conversion, whose
 * return type PostgreSQL's port headers obtain from <windows.h>.  The engine
 * never calls it; the declaration needs only the type.
 */
typedef wchar_t WCHAR;
#endif

typedef unsigned int Oid;

#define InvalidOid ((Oid) 0)
#define OidIsValid(objectId) ((bool) ((objectId) != InvalidOid))

/* Only pointers to text appear in the imported declarations. */
struct varlena
{
	char		vl_len_[4];
	char		vl_dat[FLEXIBLE_ARRAY_MEMBER];
};
typedef struct varlena text;

#include "utils/palloc.h"

#endif							/* UQA_PG_REGEX_POSTGRES_H */
