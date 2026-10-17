/*-------------------------------------------------------------------------
 *
 * regc_pg_locale.c
 *	  ctype functions adapted to work on pg_wchar (a/k/a chr), and the
 *	  character-class member lists they define.
 *
 * This file is #included by regcomp.c in place of PostgreSQL's file of the
 * same name; it's not meant to compile standalone.
 *
 * Portions Copyright (c) 2023-2026, Cognica, Inc.
 * Portions Copyright (c) 1996-2025, PostgreSQL Global Development Group
 * Portions Copyright (c) 1994, Regents of the University of California
 *
 * Adapted for Unified Query Algebra from src/backend/regex/regc_pg_locale.c
 * of PostgreSQL 18.4 (tag REL_18_4).  The behavior of each strategy is
 * PostgreSQL's.  The adaptation changes where state lives and which
 * strategies exist:
 *
 * - PostgreSQL stores the active strategy in static variables because a
 *	 backend runs one regular expression operation at a time.  UQA runs
 *	 operations concurrently on several threads, so the strategy is kept in
 *	 the per-thread frame of the current engine operation.
 *
 * - pg_set_regex_collation() receives a locale token that the Rust wrapper
 *	 chose after applying PostgreSQL's collation checks, including the
 *	 rejection of invalid and nondeterministic collations.  A value that is
 *	 not one of the four tokens is an internal error, raised through the
 *	 operation's error frame.
 *
 * - The C, builtin and ICU strategies are provided.  UQA defines no libc
 *	 collation other than C and POSIX, which use the C strategy, so the
 *	 PG_REGEX_STRATEGY_LIBC_WIDE and PG_REGEX_STRATEGY_LIBC_1BYTE strategies
 *	 have no counterpart.  The builtin strategy calls PostgreSQL's
 *	 unicode_category.c and unicode_case.c unchanged.  The ICU strategy calls
 *	 ICU4C-compatible u_is*() and u_to*() functions implemented by the Rust
 *	 wrapper over ICU4X data.
 *
 * - PostgreSQL caches the cvec of each ctype class per backend in malloc'd
 *	 memory.  Here the Rust wrapper keeps each complete cvec once per process
 *	 in immutable storage, so a class reference allocates nothing, as on a
 *	 hit in PostgreSQL's cache.
 *
 *-------------------------------------------------------------------------
 */

#include "common/unicode_case.h"
#include "common/unicode_category.h"
#include "uqa_pg_regex.h"

/*
 * Hard-wired character properties for C locale
 */
#define PG_ISDIGIT	0x01
#define PG_ISALPHA	0x02
#define PG_ISALNUM	(PG_ISDIGIT | PG_ISALPHA)
#define PG_ISUPPER	0x04
#define PG_ISLOWER	0x08
#define PG_ISGRAPH	0x10
#define PG_ISPRINT	0x20
#define PG_ISPUNCT	0x40
#define PG_ISSPACE	0x80

static const unsigned char pg_char_properties[128] = {
	 /* NUL */ 0,
	 /* ^A */ 0,
	 /* ^B */ 0,
	 /* ^C */ 0,
	 /* ^D */ 0,
	 /* ^E */ 0,
	 /* ^F */ 0,
	 /* ^G */ 0,
	 /* ^H */ 0,
	 /* ^I */ PG_ISSPACE,
	 /* ^J */ PG_ISSPACE,
	 /* ^K */ PG_ISSPACE,
	 /* ^L */ PG_ISSPACE,
	 /* ^M */ PG_ISSPACE,
	 /* ^N */ 0,
	 /* ^O */ 0,
	 /* ^P */ 0,
	 /* ^Q */ 0,
	 /* ^R */ 0,
	 /* ^S */ 0,
	 /* ^T */ 0,
	 /* ^U */ 0,
	 /* ^V */ 0,
	 /* ^W */ 0,
	 /* ^X */ 0,
	 /* ^Y */ 0,
	 /* ^Z */ 0,
	 /* ^[ */ 0,
	 /* ^\ */ 0,
	 /* ^] */ 0,
	 /* ^^ */ 0,
	 /* ^_ */ 0,
	 /* */ PG_ISPRINT | PG_ISSPACE,
	 /* !  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* "  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* #  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* $  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* %  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* &  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* '  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* (  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* )  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* *  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* +  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* ,  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* -  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* .  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* /  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* 0  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 1  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 2  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 3  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 4  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 5  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 6  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 7  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 8  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* 9  */ PG_ISDIGIT | PG_ISGRAPH | PG_ISPRINT,
	 /* :  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* ;  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* <  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* =  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* >  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* ?  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* @  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* A  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* B  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* C  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* D  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* E  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* F  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* G  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* H  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* I  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* J  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* K  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* L  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* M  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* N  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* O  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* P  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* Q  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* R  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* S  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* T  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* U  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* V  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* W  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* X  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* Y  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* Z  */ PG_ISALPHA | PG_ISUPPER | PG_ISGRAPH | PG_ISPRINT,
	 /* [  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* \  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* ]  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* ^  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* _  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* `  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* a  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* b  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* c  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* d  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* e  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* f  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* g  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* h  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* i  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* j  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* k  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* l  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* m  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* n  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* o  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* p  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* q  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* r  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* s  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* t  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* u  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* v  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* w  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* x  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* y  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* z  */ PG_ISALPHA | PG_ISLOWER | PG_ISGRAPH | PG_ISPRINT,
	 /* {  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* |  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* }  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* ~  */ PG_ISGRAPH | PG_ISPRINT | PG_ISPUNCT,
	 /* DEL */ 0
};

/* ASCII case mapping, as PostgreSQL's port.h defines it */
static inline unsigned char
pg_ascii_toupper(unsigned char ch)
{
	if (ch >= 'a' && ch <= 'z')
		ch += 'A' - 'a';
	return ch;
}

static inline unsigned char
pg_ascii_tolower(unsigned char ch)
{
	if (ch >= 'A' && ch <= 'Z')
		ch += 'a' - 'A';
	return ch;
}


/*
 * pg_set_regex_collation: set collation for these functions to obey
 *
 * This is called when beginning compilation or execution of a regexp.  The
 * argument is a UQA locale token; see uqa_pg_regex.h.
 */
void
pg_set_regex_collation(Oid collation)
{
	uqa_pg_regex_select_locale(collation);
}

/*
 * The builtin provider's "posix" variants apply when casemap_full is off,
 * that is, for C.UTF-8 rather than PG_UNICODE_FAST.
 */
#define BUILTIN_POSIX(locale) ((locale) == UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8)

static int
pg_wc_isdigit(pg_wchar c)
{
	Oid			locale = uqa_pg_regex_current_locale();

	switch (locale)
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISDIGIT));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_isdigit(c, BUILTIN_POSIX(locale));
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_isdigit(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static int
pg_wc_isalpha(pg_wchar c)
{
	switch (uqa_pg_regex_current_locale())
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISALPHA));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_isalpha(c);
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_isalpha(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static int
pg_wc_isalnum(pg_wchar c)
{
	Oid			locale = uqa_pg_regex_current_locale();

	switch (locale)
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISALNUM));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_isalnum(c, BUILTIN_POSIX(locale));
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_isalnum(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static int
pg_wc_isword(pg_wchar c)
{
	/* We define word characters as alnum class plus underscore */
	if (c == CHR('_'))
		return 1;
	return pg_wc_isalnum(c);
}

static int
pg_wc_isupper(pg_wchar c)
{
	switch (uqa_pg_regex_current_locale())
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISUPPER));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_isupper(c);
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_isupper(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static int
pg_wc_islower(pg_wchar c)
{
	switch (uqa_pg_regex_current_locale())
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISLOWER));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_islower(c);
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_islower(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static int
pg_wc_isgraph(pg_wchar c)
{
	switch (uqa_pg_regex_current_locale())
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISGRAPH));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_isgraph(c);
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_isgraph(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static int
pg_wc_isprint(pg_wchar c)
{
	switch (uqa_pg_regex_current_locale())
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISPRINT));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_isprint(c);
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_isprint(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static int
pg_wc_ispunct(pg_wchar c)
{
	Oid			locale = uqa_pg_regex_current_locale();

	switch (locale)
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISPUNCT));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_ispunct(c, BUILTIN_POSIX(locale));
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_ispunct(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static int
pg_wc_isspace(pg_wchar c)
{
	switch (uqa_pg_regex_current_locale())
	{
		case UQA_PG_REGEX_LOCALE_C:
			return (c <= (pg_wchar) 127 &&
					(pg_char_properties[c] & PG_ISSPACE));
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return pg_u_isspace(c);
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_isspace(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static pg_wchar
pg_wc_toupper(pg_wchar c)
{
	switch (uqa_pg_regex_current_locale())
	{
		case UQA_PG_REGEX_LOCALE_C:
			if (c <= (pg_wchar) 127)
				return pg_ascii_toupper((unsigned char) c);
			return c;
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return unicode_uppercase_simple(c);
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_toupper(c);
	}
	uqa_pg_regex_invalid_locale_state();
}

static pg_wchar
pg_wc_tolower(pg_wchar c)
{
	switch (uqa_pg_regex_current_locale())
	{
		case UQA_PG_REGEX_LOCALE_C:
			if (c <= (pg_wchar) 127)
				return pg_ascii_tolower((unsigned char) c);
			return c;
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
			return unicode_lowercase_simple(c);
		case UQA_PG_REGEX_LOCALE_ICU:
			return uqa_pg_regex_icu_tolower(c);
	}
	uqa_pg_regex_invalid_locale_state();
}


/*
 * These functions provide the results of probing the ctype behavior for all
 * character codes of interest in the active locale, as a "struct cvec" that
 * the Rust wrapper keeps in immutable process-wide storage.  Callers must not
 * free or modify the result.
 */

typedef int (*pg_wc_probefunc) (pg_wchar c);

/*
 * Given a probe function (e.g., pg_wc_isalpha) get a struct cvec for all
 * chrs satisfying the probe function.  The active collation is the one
 * previously set by pg_set_regex_collation.  The lists are built once per
 * process, in storage outside any operation's memory context, as PostgreSQL
 * builds its cache with malloc.
 */
static struct cvec *
pg_ctype_get_cache(pg_wc_probefunc probefunc, int cclasscode)
{
	Oid			locale = uqa_pg_regex_current_locale();
	pg_wchar	max_chr;
	int			result_cclasscode = cclasscode;

	/*
	 * Decide how many character codes we ought to look through.  In general
	 * we don't go past MAX_SIMPLE_CHR; chr codes above that are handled at
	 * runtime using the "high colormap" mechanism.  However, in C locale
	 * there's no need to go further than 127.
	 *
	 * If it's not MAX_SIMPLE_CHR that's constraining the search, mark the
	 * output cvec as not having any locale-dependent behavior, since there
	 * will be no need to do any run-time locale checks.
	 */
	switch (locale)
	{
		case UQA_PG_REGEX_LOCALE_C:
#if MAX_SIMPLE_CHR >= 127
			max_chr = (pg_wchar) 127;
			result_cclasscode = -1;
#else
			max_chr = (pg_wchar) MAX_SIMPLE_CHR;
#endif
			break;
		case UQA_PG_REGEX_LOCALE_BUILTIN_C_UTF8:
		case UQA_PG_REGEX_LOCALE_BUILTIN_UNICODE:
		case UQA_PG_REGEX_LOCALE_ICU:
			max_chr = (pg_wchar) MAX_SIMPLE_CHR;
			break;
		default:
			uqa_pg_regex_invalid_locale_state();
	}

	return (struct cvec *) uqa_pg_regex_class_cvec(locale, cclasscode,
												   result_cclasscode,
												   probefunc, max_chr);
}

/* The size and alignment of the process-wide cvecs that Rust allocates. */
size_t
uqa_pg_regex_cvec_size(void)
{
	return sizeof(struct cvec);
}

size_t
uqa_pg_regex_cvec_alignment(void)
{
	struct alignment_probe
	{
		char		leading;
		struct cvec cv;
	};

	return offsetof(struct alignment_probe, cv);
}

/*
 * Fill a process-wide cvec.  Its arrays have exactly the needed length, as
 * PostgreSQL trims its cached arrays, and empty arrays are NULL.
 */
void
uqa_pg_regex_fill_cvec(void *storage, const pg_wchar *chrs, int nchrs,
					   const pg_wchar *ranges, int nranges, int cclasscode)
{
	struct cvec *cv = (struct cvec *) storage;

	cv->nchrs = nchrs;
	cv->chrspace = nchrs;
	cv->chrs = (chr *) chrs;
	cv->nranges = nranges;
	cv->rangespace = nranges;
	cv->ranges = (chr *) ranges;
	cv->cclasscode = cclasscode;
}
