/*
 * np2mem.c -- HOOTRIP glue
 *
 * Flat real-mode memory back-end for the vendored i286c core. Replaces
 * np2kai/i286c/cpumem.c, dropping all VRAM / text-RAM / EGC / EMS / ROM /
 * GRCG banking and Cirrus-GD5430 special regions: every physical access
 * simply lands in a single 2 MiB `mem[]` array.
 *
 * Address model
 * -------------
 * The i286c core hands these accessors a *physical* address that already
 * includes the segment base (e.g. CS_BASE + IP). We honour the CPU's
 * address mask (i286core.s.adrsmask -- 0x000FFFFF in 8086/real mode,
 * 0x00FFFFFF for the 286) exactly as upstream memp_* does, then index
 * `mem[phys & 0x1FFFFF]`. 16/32-bit accesses are little-endian and may be
 * unaligned; each constituent byte is independently wrapped to the array
 * bound so an access at the very top of memory can never read/write out of
 * bounds (it wraps within the 2 MiB space, matching a real 21-bit bus).
 *
 * cpumem.h declares these as the machine's memory API; the CPU reaches them
 * through the i286_memoryread / i286_memorywrite macros in i286c.h, which are
 * defined to memp_read8/16/32 and memp_write8/16/32.
 */

#include <compiler.h>
#include <cpucore.h>     /* I286CORE i286core (for adrsmask), cpumem.h */

/* 2 MiB flat space (000000-1FFFFF): main RAM + the PC-98 upper/VRAM window
 * region, all treated as plain RAM here. Defined here (upstream defines it
 * in cpumem.c). */
UINT8 mem[0x200000];

#define MEM_ARRMASK 0x001FFFFF

/* physical address after honouring the CPU address mask, clamped to array */
#define PHYS(a) (((a) & i286core.s.adrsmask) & MEM_ARRMASK)


/* ---- Physical space (byte / word / dword) ---- */

REG8 MEMCALL memp_read8(UINT32 address) {

	return mem[PHYS(address)];
}

REG16 MEMCALL memp_read16(UINT32 address) {

	UINT32 base = address & i286core.s.adrsmask;
	return (REG16)(mem[base & MEM_ARRMASK] |
	              (mem[(base + 1) & MEM_ARRMASK] << 8));
}

UINT32 MEMCALL memp_read32(UINT32 address) {

	UINT32 base = address & i286core.s.adrsmask;
	return  (UINT32)mem[ base      & MEM_ARRMASK]        |
	       ((UINT32)mem[(base + 1) & MEM_ARRMASK] << 8)  |
	       ((UINT32)mem[(base + 2) & MEM_ARRMASK] << 16) |
	       ((UINT32)mem[(base + 3) & MEM_ARRMASK] << 24);
}

void MEMCALL memp_write8(UINT32 address, REG8 value) {

	mem[PHYS(address)] = (UINT8)value;
}

void MEMCALL memp_write16(UINT32 address, REG16 value) {

	UINT32 base = address & i286core.s.adrsmask;
	mem[ base      & MEM_ARRMASK] = (UINT8)value;
	mem[(base + 1) & MEM_ARRMASK] = (UINT8)(value >> 8);
}

void MEMCALL memp_write32(UINT32 address, UINT32 value) {

	UINT32 base = address & i286core.s.adrsmask;
	mem[ base      & MEM_ARRMASK] = (UINT8)value;
	mem[(base + 1) & MEM_ARRMASK] = (UINT8)(value >> 8);
	mem[(base + 2) & MEM_ARRMASK] = (UINT8)(value >> 16);
	mem[(base + 3) & MEM_ARRMASK] = (UINT8)(value >> 24);
}


/* ---- Physical block copy ---- */

void MEMCALL memp_reads(UINT32 address, void *dat, UINT leng) {

	UINT8  *out  = (UINT8 *)dat;
	UINT32  base = address & i286core.s.adrsmask;

	if (((base & MEM_ARRMASK) + leng) <= (MEM_ARRMASK + 1)) {
		CopyMemory(out, mem + (base & MEM_ARRMASK), leng);
	}
	else {
		while (leng--) {
			*out++ = mem[base & MEM_ARRMASK];
			base++;
		}
	}
}

void MEMCALL memp_writes(UINT32 address, const void *dat, UINT leng) {

	const UINT8 *in   = (const UINT8 *)dat;
	UINT32       base = address & i286core.s.adrsmask;

	if (((base & MEM_ARRMASK) + leng) <= (MEM_ARRMASK + 1)) {
		CopyMemory(mem + (base & MEM_ARRMASK), in, leng);
	}
	else {
		while (leng--) {
			mem[base & MEM_ARRMASK] = *in++;
			base++;
		}
	}
}


/* ---- Logical (segment:offset) space, BIOS-style helpers ---- *
 * Offset arithmetic wraps within the 64 KiB segment (LOW16), exactly like
 * upstream memr_*; the resulting physical address then goes through PHYS(). */

REG8 MEMCALL memr_read8(UINT seg, UINT off) {

	return memp_read8((seg << 4) + LOW16(off));
}

REG16 MEMCALL memr_read16(UINT seg, UINT off) {

	return memp_read16((seg << 4) + LOW16(off));
}

void MEMCALL memr_write8(UINT seg, UINT off, REG8 value) {

	memp_write8((seg << 4) + LOW16(off), value);
}

void MEMCALL memr_write16(UINT seg, UINT off, REG16 value) {

	memp_write16((seg << 4) + LOW16(off), value);
}

void MEMCALL memr_reads(UINT seg, UINT off, void *dat, UINT leng) {

	UINT8  *out = (UINT8 *)dat;
	UINT32  adrs = seg << 4;

	off = LOW16(off);
	while (leng--) {
		*out++ = memp_read8(adrs + off);
		off = LOW16(off + 1);
	}
}

void MEMCALL memr_writes(UINT seg, UINT off, const void *dat, UINT leng) {

	const UINT8 *in = (const UINT8 *)dat;
	UINT32       adrs = seg << 4;

	off = LOW16(off);
	while (leng--) {
		memp_write8(adrs + off, *in++);
		off = LOW16(off + 1);
	}
}


/* ---- Memory-map reconfiguration entry points ---- *
 * Upstream these repoint the banked accessor tables (VRAM/ROM windows).
 * The flat model has no banks, so they are no-ops -- provided only so the
 * cpumem.h API surface stays complete for future callers. */

void MEMCALL memm_arch(UINT type) {

	UNUSED(type);
}

void MEMCALL memm_vram(UINT operate) {

	UNUSED(operate);
}
