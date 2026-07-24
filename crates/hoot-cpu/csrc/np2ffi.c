/*
 * np2ffi.c -- hootrip FFI shim over the vendored NP2 i286c core.
 *
 * Provides clean C accessors so the Rust `Np2Cpu` never has to mirror the
 * `I286CORE` struct layout. Segment-register writes update the core's cached
 * base/fix fields exactly as the real-mode segment-load path does (mirrors the
 * verified setup in vendor/np2/glue/steptest.c). Compiled alongside the
 * vendored core by build.rs; NOT part of the pristine vendor tree.
 */

#include <np2types.h>
#include <cpucore.h>

/* Register indices -- MUST match Rust `hoot_cpu::Reg16` discriminant order. */
enum {
    R_AX = 0, R_BX, R_CX, R_DX, R_SI, R_DI, R_BP, R_SP,
    R_CS, R_DS, R_ES, R_SS, R_IP, R_FLAGS
};

UINT16 np2_reg_get(int idx) {
    switch (idx) {
        case R_AX: return i286core.s.r.w.ax;
        case R_BX: return i286core.s.r.w.bx;
        case R_CX: return i286core.s.r.w.cx;
        case R_DX: return i286core.s.r.w.dx;
        case R_SI: return i286core.s.r.w.si;
        case R_DI: return i286core.s.r.w.di;
        case R_BP: return i286core.s.r.w.bp;
        case R_SP: return i286core.s.r.w.sp;
        case R_CS: return i286core.s.r.w.cs;
        case R_DS: return i286core.s.r.w.ds;
        case R_ES: return i286core.s.r.w.es;
        case R_SS: return i286core.s.r.w.ss;
        case R_IP: return i286core.s.r.w.ip;
        case R_FLAGS: return i286core.s.r.w.flag;
    }
    return 0;
}

void np2_reg_set(int idx, UINT16 v) {
    UINT32 base = (UINT32)v << 4;
    switch (idx) {
        case R_AX: i286core.s.r.w.ax = v; break;
        case R_BX: i286core.s.r.w.bx = v; break;
        case R_CX: i286core.s.r.w.cx = v; break;
        case R_DX: i286core.s.r.w.dx = v; break;
        case R_SI: i286core.s.r.w.si = v; break;
        case R_DI: i286core.s.r.w.di = v; break;
        case R_BP: i286core.s.r.w.bp = v; break;
        case R_SP: i286core.s.r.w.sp = v; break;
        case R_IP: i286core.s.r.w.ip = v; break;
        case R_FLAGS: i286core.s.r.w.flag = v; break;
        /* Real-mode segment loads recompute the cached base (and _fix for the
         * data/stack segments used by memory operands). */
        case R_CS: i286core.s.r.w.cs = v; i286core.s.cs_base = base; break;
        case R_DS: i286core.s.r.w.ds = v; i286core.s.ds_base = base; i286core.s.ds_fix = base; break;
        case R_ES: i286core.s.r.w.es = v; i286core.s.es_base = base; break;
        case R_SS: i286core.s.r.w.ss = v; i286core.s.ss_base = base; i286core.s.ss_fix = base; break;
    }
}

void np2_set_cs_ip(UINT16 cs, UINT16 ip) {
    i286core.s.r.w.cs = cs;
    i286core.s.cs_base = (UINT32)cs << 4;
    i286core.s.r.w.ip = ip;
}

void np2_set_ss_sp(UINT16 ss, UINT16 sp) {
    i286core.s.r.w.ss = ss;
    i286core.s.ss_base = (UINT32)ss << 4;
    i286core.s.ss_fix = (UINT32)ss << 4;
    i286core.s.r.w.sp = sp;
}

/* Physical PC (cs_base + ip), masked -- used to peek the next opcode. */
UINT32 np2_pc_phys(void) {
    return (i286core.s.cs_base + i286core.s.r.w.ip) & i286core.s.adrsmask;
}

void np2_set_adrsmask(UINT32 m) { i286core.s.adrsmask = m; }

/* V30 vs 286 opcode semantics: selects which single-step entry np2_step uses. */
static int s_v30 = 0;
void np2_set_v30(int on) { s_v30 = on ? 1 : 0; }

void np2_init(void) { i286c_initialize(); }
void np2_reset(void) { i286c_reset(); }
void np2_setextsize(UINT32 sz) { i286c_setextsize(sz); }

/* Deliver an interrupt (push FLAGS/CS/IP, vector via IVT, clear IF/TF). */
void np2_interrupt(UINT8 vect) { i286c_interrupt(vect); }

/* Execute one instruction; return the cycle count it consumed.
 * remainclock is set to 0 first, so after the step it holds -(cycles). */
SINT32 np2_step(void) {
    i286core.s.remainclock = 0;
    if (s_v30) {
        v30c_step();
    } else {
        i286c_step();
    }
    return -i286core.s.remainclock;
}

/* Pointer to the flat 2 MB physical memory image (mem[0x200000]). */
UINT8 *np2_mem(void) { return mem; }
