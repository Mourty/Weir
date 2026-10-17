/* config.h for building LAME with the cc crate on everything but Visual
 * C++, which uses LAME's own configMS.h. It sets what LAME's configure
 * script finds on Linux, for any C99 compiler. */
#ifndef LAME_CONFIG_H
#define LAME_CONFIG_H

#define STDC_HEADERS 1
#define HAVE_ERRNO_H 1
#define HAVE_FCNTL_H 1
#define HAVE_LIMITS_H 1
#define HAVE_STDINT_H 1
#define HAVE_INTTYPES_H 1

typedef float ieee754_float32_t;
typedef double ieee754_float64_t;
typedef long double ieee854_float80_t;

#define LAME_LIBRARY_BUILD 1
#define PACKAGE "lame"
#define PACKAGE_NAME "lame"
#define PACKAGE_VERSION "3.100"
#define VERSION "3.100"

/* What configure turns on for x86 and PowerPC processors. */
#if defined(__x86_64__) || defined(__i386__) || defined(__powerpc__)
#define TAKEHIRO_IEEE754_HACK 1
#define USE_FAST_LOG 1
#endif
#if defined(__SSE__)
#define HAVE_XMMINTRIN_H 1
#endif

#endif /* LAME_CONFIG_H */
