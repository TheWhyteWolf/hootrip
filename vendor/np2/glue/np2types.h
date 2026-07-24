/*
 * np2types.h -- HOOTRIP glue
 *
 * Minimal, self-contained replacement for Neko Project II's <compiler.h>
 * base layer, providing exactly the typedefs and macros the vendored i286c
 * core needs to build standalone on Linux (real-mode / 286 / V30 integer
 * subset only -- no SDL, glib, Windows or protected-mode dependencies).
 *
 * The typedefs below match np2kai/compiler_base.h byte-for-byte in size and
 * signedness. The calling-convention macros (CPUCALL/MEMCALL/IOINPCALL/
 * IOOUTCALL/DMACCALL/INLINE) all expand to nothing: on Linux x86_64 NP2's
 * own compiler_base.h resolves FASTCALL (and hence all of these) to empty.
 */

#ifndef HOOTRIP_NP2TYPES_H
#define HOOTRIP_NP2TYPES_H

#include <stdio.h>
#include <stdlib.h>
#include <stddef.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>
#include <limits.h>

/* ---- byte order (target is little-endian PC-98) ---- */
#ifndef BYTESEX_LITTLE
#define BYTESEX_LITTLE
#endif

/* ---- fixed-size integer typedefs (match compiler_base.h exactly) ---- */
typedef int          INT;
typedef INT          SINT;
typedef unsigned int UINT;

typedef int8_t   INT8;
typedef INT8     SINT8;
typedef uint8_t  UINT8;
typedef int16_t  INT16;
typedef INT16    SINT16;
typedef uint16_t UINT16;
typedef int32_t  INT32;
typedef INT32    SINT32;
typedef uint32_t UINT32;
typedef int64_t  INT64;
typedef INT64    SINT64;
typedef uint64_t UINT64;

/* variable width */
typedef size_t    SIZET;
typedef intptr_t  INTPTR;
typedef uintptr_t UINTPTR;
typedef intptr_t  INT_PTR;
typedef uintptr_t UINT_PTR;

/* CPU register operands */
typedef uint8_t  REG8;
typedef uint16_t REG16;

/* Windows-ish aliases that NP2 code sometimes reaches for */
typedef uint8_t  BYTE;
typedef uint16_t WORD;
typedef uint32_t DWORD;
typedef bool     BOOL;
typedef bool     BRESULT;

#ifndef TRUE
#define TRUE  (1 == 1)
#endif
#ifndef FALSE
#define FALSE (1 == 0)
#endif

/* ---- calling conventions: empty on Linux x86_64 (FASTCALL == nothing) ---- */
#ifndef CDECL
#define CDECL
#endif
#ifndef STDCALL
#define STDCALL
#endif
#ifndef FASTCALL
#define FASTCALL
#endif
#ifndef CPUCALL
#define CPUCALL
#endif
#ifndef MEMCALL
#define MEMCALL
#endif
#ifndef DMACCALL
#define DMACCALL
#endif
#ifndef IOINPCALL
#define IOINPCALL
#endif
#ifndef IOOUTCALL
#define IOOUTCALL
#endif
#ifndef SOUNDCALL
#define SOUNDCALL
#endif
#ifndef VRAMCALL
#define VRAMCALL
#endif
#ifndef SCRNCALL
#define SCRNCALL
#endif
#ifndef INLINE
#define INLINE
#endif
#ifndef UNUSED
#define UNUSED(v) (void)(v)
#endif

/* ---- REG8/REG16 fallbacks used by common.h-style macros ---- */
#ifndef REG8
#define REG8  UINT8
#endif
#ifndef REG16
#define REG16 UINT16
#endif

/* ---- little/big endian load/store helpers (from np2kai/common.h) ---- */
#ifndef LOADINTELWORD
#define LOADINTELWORD(a)      (((UINT16)((UINT8*)(a))[0]) | ((UINT16)(((UINT8*)(a))[1]) << 8))
#endif
#ifndef LOADINTELDWORD
#define LOADINTELDWORD(a)     (((UINT32)(((UINT8*)(a))[0]))       |  \
                               ((UINT32)(((UINT8*)(a))[1]) << 8)  |  \
                               ((UINT32)(((UINT8*)(a))[2]) << 16) |  \
                               ((UINT32)(((UINT8*)(a))[3]) << 24))
#endif
#ifndef STOREINTELWORD
#define STOREINTELWORD(a, b)  *(((UINT8*)(a))+0) = (UINT8)((b));      \
                              *(((UINT8*)(a))+1) = (UINT8)((b)>>8)
#endif
#ifndef STOREINTELDWORD
#define STOREINTELDWORD(a, b) *(((UINT8*)(a))+0) = (UINT8)((b));      \
                              *(((UINT8*)(a))+1) = (UINT8)((b)>>8);   \
                              *(((UINT8*)(a))+2) = (UINT8)((b)>>16);  \
                              *(((UINT8*)(a))+3) = (UINT8)((b)>>24)
#endif

/* ---- low/high field extraction ---- */
#ifndef LOW8
#define LOW8(a)   ((UINT8)(a))
#endif
#ifndef LOW16
#define LOW16(a)  ((UINT16)(a))
#endif
#ifndef HIGH16
#define HIGH16(a) (((UINT32)(a)) >> 16)
#endif

/* ---- min/max/nelements ---- */
#ifndef MAX
#define MAX(a, b) (((a) > (b)) ? (a) : (b))
#endif
#ifndef MIN
#define MIN(a, b) (((a) < (b)) ? (a) : (b))
#endif
#ifndef NELEMENTS
#define NELEMENTS(a) ((int)(sizeof(a) / sizeof(a[0])))
#endif

/* ---- memory helpers (Win32-style names NP2 uses) ---- */
#ifndef ZeroMemory
#define ZeroMemory(d, z)    memset((d), 0, (z))
#endif
#ifndef CopyMemory
#define CopyMemory(d, s, z) memcpy((d), (s), (z))
#endif
#ifndef FillMemory
#define FillMemory(d, z, c) memset((d), (c), (z))
#endif

/* ---- allocation (np2kai/common/_memory.h, generic variant) ---- */
#ifndef _MALLOC
#define _MALLOC(a, b) malloc(a)
#endif
#ifndef _MFREE
#define _MFREE(a)     free(a)
#endif

/* ---- trace: no-op (np2kai/x/trace.h non-debug variant) ---- */
#ifndef TRACEOUT
#define TRACEOUT(a)
#endif

#ifndef MAX_PATH
#define MAX_PATH 4096
#endif

#endif /* HOOTRIP_NP2TYPES_H */
