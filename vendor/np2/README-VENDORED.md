# Vendored: Neko Project II (np2kai) i286c CPU core

This directory contains the **i286c** real-mode 80286 / V30 CPU core extracted
from Neko Project II (np2kai) and made to build as a self-contained C static
library for the **hootrip** project. It is driven from Rust via FFI to execute
PC-98 real-mode DOS sound drivers. No SDL, glib, Windows, protected-mode, IA-32,
async-CPU, trap, or BMS code is included or enabled.

## Provenance

- **Upstream:** np2kai (Neko Project II kai)
- **Commit:** `e2dc9046aa5c786fcfbfb87e883457e421026e31`
- **Upstream path of the core:** `i286c/`
- **License:** BSD 3-clause, "Copyright (c) 1999-2025, NP2 developer team".
  Full text in [`LICENSE-NP2.txt`](LICENSE-NP2.txt) (copied verbatim from
  `np2kai/LICENSES/LICENSE.TXT`; note its first line is a Shift_JIS/CP932
  comment, preserved as-is).

## Layout

```
cpu/      Vendored i286c core (BYTE-IDENTICAL to upstream i286c/ -- zero patches)
glue/     hootrip-authored glue (memory, I/O, base types, acceptance test)
include/  Stub headers so the core's <compiler.h>/<pccore.h>/<io/iocore.h>/
          <mem/dmax86.h>/<mem/dmav30.h>/<bios/bios.h> resolve to minimal decls
```

### Vendored core files (`cpu/`) — compiled

| File          | Role |
|---------------|------|
| `i286c.c`     | Public API: init/reset/step/run, `i286core`, interrupts |
| `i286c_mn.c`  | Main opcode handlers (mnemonic table) |
| `i286c_0f.c`  | `0F`-prefixed 286 opcodes |
| `i286c_8x.c`  | `80-83` group (ALU imm) |
| `i286c_ea.c`  | Effective-address / ModRM decode, `i286c_selector` |
| `i286c_f6.c`  | `F6/F7` group (mul/div/test/neg/not) |
| `i286c_fe.c`  | `FE/FF` group (inc/dec/call/jmp/push) |
| `i286c_rp.c`  | String/repeat primitives (movs/stos/cmps/scas...) |
| `i286c_sf.c`  | Shift/rotate group |
| `v30patch.c`  | NEC V30 (µPD70116) instruction variants (`v30c`, `v30c_step`) |

### Vendored core files (`cpu/`) — headers/macros (included, not compiled)

`cpucore.h` (the `I286CORE i286core` struct + public prototypes), `i286c.h`,
`cpumem.h`, `v30patch.h`, `i286c.mcr`, `i286c_sf.mcr`.

### hootrip glue (`glue/`)

| File           | Provides |
|----------------|----------|
| `np2types.h`   | Base typedefs (UINT8/16/32, SINT8/16/32, REG8/16, BOOL) + empty calling-convention macros + LOADINTEL*/STOREINTEL*/LOW*/MIN/MAX/ZeroMemory/CopyMemory/_MALLOC/TRACEOUT + `#define BYTESEX_LITTLE`. Sizes/signedness match upstream `compiler_base.h` exactly. |
| `np2mem.c`     | Flat real-mode memory: `UINT8 mem[0x200000]` and all cpumem.h accessors |
| `np2io.c`      | `iocore_*` port I/O forwarding to overridable hooks + machine stubs |
| `steptest.c`   | Standalone acceptance test (`main`) |

## Core patches

**None.** All 16 files in `cpu/` are byte-for-byte identical to upstream
(`diff -q` clean against the commit above). Every adaptation lives in the stub
headers (`include/`) and glue (`glue/`), so no `/* HOOTRIP: ... */` markers are
present in the core. If the core is ever re-vendored from a newer commit, no
manual re-patching is required.

### How the stubs stand in for the real machine

- `<compiler.h>` → `#include <np2types.h>` (drops the SDL/glib/Windows base).
- `<pccore.h>` → empty (its only uses in the core sit inside a fully
  commented-out `SUPPORT_ASYNC_CPU` block in `i286c.c`).
- `<io/iocore.h>` → declares `iocore_*`, a one-field `dmac` (`.working`), and
  `#define PICEXISTINTR (0)` (no 8259 emulated ⇒ no IRQ ever pending).
- `<mem/dmax86.h>` / `<mem/dmav30.h>` → declare `dmax86()` / `dmav30()`.
- `<bios/bios.h>` → declares `biosfunc()` (BIOS-ROM call trap; never entered).
- `trap/steptrap.h` and `trap/inttrap.h` are **not** needed — their `#include`s
  are guarded by `ENABLE_TRAP`, which we do not define.

## Compile configuration

- **Define:** `BYTESEX_LITTLE` (also self-defined in `np2types.h`; the `-D` is
  belt-and-suspenders and matches the spec).
- **Explicitly NOT defined:** `SUPPORT_ASYNC_CPU`, `ENABLE_TRAP`, `CPUCORE_IA32`,
  `SUPPORT_BMS`, `SUPPORT_PC9821`, `SUPPORT_CL_GD5430`, `MEMOPTIMIZE`, `X11`,
  `arm`/`__arm__`, or any protected-mode/IA-32 flag.
- **Include dirs (order matters):** `-Iglue -Iinclude -Icpu`
  (`glue` first so `<np2types.h>` resolves; `cpu` supplies `<cpucore.h>` and the
  quote-included `i286c.h`/`.mcr`/`cpumem.h`).

### Sources compiled (10 core + 2 glue)

```
cpu/i286c.c cpu/i286c_0f.c cpu/i286c_8x.c cpu/i286c_ea.c cpu/i286c_f6.c
cpu/i286c_fe.c cpu/i286c_mn.c cpu/i286c_rp.c cpu/i286c_sf.c cpu/v30patch.c
glue/np2mem.c glue/np2io.c
```

### Exact build command (static library)

```sh
cd vendor/np2
CFLAGS="-DBYTESEX_LITTLE -Iglue -Iinclude -Icpu -O2 -Wall -Wno-unused -Wno-parentheses"
for f in cpu/i286c.c cpu/i286c_0f.c cpu/i286c_8x.c cpu/i286c_ea.c cpu/i286c_f6.c \
         cpu/i286c_fe.c cpu/i286c_mn.c cpu/i286c_rp.c cpu/i286c_sf.c cpu/v30patch.c \
         glue/np2mem.c glue/np2io.c; do
    gcc -c $CFLAGS "$f" -o "$(basename ${f%.c}).o"
done
ar rcs libnp2i286c.a *.o
```

### Acceptance test (build + run)

```sh
cd vendor/np2
gcc -DBYTESEX_LITTLE -Iglue -Iinclude -Icpu -O2 -Wall -Wno-unused -Wno-parentheses \
    cpu/i286c.c cpu/i286c_0f.c cpu/i286c_8x.c cpu/i286c_ea.c cpu/i286c_f6.c \
    cpu/i286c_fe.c cpu/i286c_mn.c cpu/i286c_rp.c cpu/i286c_sf.c cpu/v30patch.c \
    glue/np2mem.c glue/np2io.c glue/steptest.c -o steptest
./steptest        # exits 0 on success
```

Expected output:

```
== i286c step test ==
start  AX=0000 BX=0000 IP=0000
after MOV AX,imm : AX=1234 IP=0003
after MOV BX,AX  : BX=1234 IP=0005
after ADD AX,BX  : AX=2468 BX=1234 IP=0007
after SHR BX,4   : BX=0123 IP=000A
next opcode      : F4 (expect F4=HLT)
memp_write16/read16 @0x500 = BEEF (expect BEEF)
memr_write16/read16 40:0010 = CAFE (expect CAFE)

PASS: AX=2468 BX=0123, memory round-trips OK
```

Confirms real-mode execution (`AX=0x2468`), a 286-only immediate-count shift
(`SHR BX,4 → 0x0123`), and physical + segment:offset memory round-trips.
No external dependencies beyond libc (`steptest` links against `-lnp2i286c`
with only libc symbols undefined).

## Public API exported by the library

From the vendored core (`cpu/cpucore.h`), all in `I286CORE i286core`:

```c
void  i286c_initialize(void);
void  i286c_deinitialize(void);
void  i286c_reset(void);              /* sets CS=0xF000 IP=0xFFF0 adrsmask=0xFFFFF */
void  i286c_shut(void);
void  i286c_setextsize(UINT32 size);  /* pass 0 for none */
void  i286c_setemm(UINT frame, UINT32 addr);
void  i286c_intnum(UINT vect, REG16 IP);
void  i286c_interrupt(REG8 vect);
void  i286c(void);                    /* run until i286core.s.remainclock <= 0 */
void  i286c_step(void);               /* single instruction */
void  v30c(void);
void  v30c_step(void);
extern I286CORE i286core;             /* .s.r.w.{ax..ip}, .s.{cs,ds,es,ss}_base,
                                         .s.adrsmask, .s.remainclock, ... */
```

Register/base fields the FFI driver sets after reset:
`i286core.s.r.w.{ax,bx,cx,dx,si,di,bp,sp,cs,ds,es,ss,ip,flag}`,
`i286core.s.{cs_base,ds_base,es_base,ss_base,ss_fix,ds_fix}`,
`i286core.s.adrsmask` (real mode 8086 = `0x000FFFFF`, 286 = `0x00FFFFFF`),
`i286core.s.remainclock`.

## External symbols the glue provides (for build.rs / FFI)

Memory (`glue/np2mem.c`):

```
UINT8 mem[0x200000];
REG8   memp_read8 (UINT32);   void memp_write8 (UINT32, REG8);
REG16  memp_read16(UINT32);   void memp_write16(UINT32, REG16);
UINT32 memp_read32(UINT32);   void memp_write32(UINT32, UINT32);
void   memp_reads (UINT32, void*, UINT);   void memp_writes(UINT32, const void*, UINT);
REG8   memr_read8 (UINT, UINT);   void memr_write8 (UINT, UINT, REG8);
REG16  memr_read16(UINT, UINT);   void memr_write16(UINT, UINT, REG16);
void   memr_reads (UINT, UINT, void*, UINT);
void   memr_writes(UINT, UINT, const void*, UINT);
void   memm_arch(UINT);   void memm_vram(UINT);     /* no-ops (flat model) */
```

Port I/O + machine stubs (`glue/np2io.c`):

```
void   iocore_out8 (UINT, REG8);    REG8   iocore_inp8 (UINT);
void   iocore_out16(UINT, REG16);   REG16  iocore_inp16(UINT);
void   iocore_out32(UINT, UINT32);  UINT32 iocore_inp32(UINT);

/* Overridable I/O hooks (all default NULL: OUT no-ops, IN returns all-ones).
   Rust assigns these to route port I/O into the sound-chip emulation. */
extern void          (*hootrip_out8) (unsigned port, unsigned char  val);
extern void          (*hootrip_out16)(unsigned port, unsigned short val);
extern void          (*hootrip_out32)(unsigned port, unsigned int   val);
extern unsigned char (*hootrip_inp8) (unsigned port);
extern unsigned short(*hootrip_inp16)(unsigned port);
extern unsigned int  (*hootrip_inp32)(unsigned port);

/* Unemulated-subsystem stubs (present only so the core links): */
HOOTRIP_DMAC dmac;             /* one UINT8 field: .working, stays 0 */
void dmax86(void);             /* no-op */
void dmav30(void);             /* no-op */
UINT biosfunc(UINT32 adrs);    /* no-op, returns 0 */
```

## Notes for a Rust `build.rs`

- Use the `cc` crate with the 12 source files above, includes `glue`, `include`,
  `cpu`, and define `BYTESEX_LITTLE`. Flags `-Wno-unused -Wno-parentheses` just
  quiet upstream style; they are not required for correctness.
- Core comments are Shift_JIS/CP932; gcc/clang accept them as-is. Do not
  re-encode the files.
- Always call `i286c_reset()` first, then overwrite CS/IP/segment bases and
  `adrsmask` to point at the loaded program (reset lands at the PC-98 reset
  vector CS=0xF000:IP=0xFFF0, not your code).
- Drive one instruction at a time with `i286c_step()`, or set
  `i286core.s.remainclock` > 0 and call `i286c()` to run a burst.
