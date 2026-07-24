/*
 * np2io.c -- HOOTRIP glue
 *
 * Port I/O back-end plus the handful of machine-subsystem symbols the i286c
 * core links against but that hootrip does not emulate.
 *
 * Port I/O
 * --------
 * iocore_out8/16/32 and iocore_inp8/16/32 forward to overridable C function
 * pointers (hootrip_out8 ... hootrip_inp32). All start NULL, in which case
 * OUT is a no-op and IN returns all-ones (0xFF / 0xFFFF / 0xFFFFFFFF), the
 * conventional "no device" value. Rust (or a C test) installs real handlers
 * by assigning these globals.
 *
 * Machine stubs
 * -------------
 *   dmac       - global read by the CPU run loop (only .working); left 0 so
 *                the DMA-servicing path is never taken.
 *   dmax86()   - i286c per-instruction DMA hook: no-op.
 *   dmav30()   - v30patch per-instruction DMA hook: no-op.
 *   biosfunc() - PC-98 BIOS-ROM call trap from the NOP handler: no-op, 0.
 */

#include <compiler.h>
#include <io/iocore.h>
#include <mem/dmax86.h>
#include <mem/dmav30.h>
#include <bios/bios.h>

/* ---- overridable I/O hooks (installed by Rust / test harness) ---- */
void          (*hootrip_out8) (unsigned port, unsigned char  val) = NULL;
void          (*hootrip_out16)(unsigned port, unsigned short val) = NULL;
void          (*hootrip_out32)(unsigned port, unsigned int   val) = NULL;
unsigned char (*hootrip_inp8) (unsigned port)                     = NULL;
unsigned short(*hootrip_inp16)(unsigned port)                     = NULL;
unsigned int  (*hootrip_inp32)(unsigned port)                     = NULL;


/* ---- iocore entry points called by the CPU core ---- */

void IOOUTCALL iocore_out8(UINT port, REG8 dat) {

	if (hootrip_out8) {
		hootrip_out8((unsigned)port, (unsigned char)dat);
	}
}

REG8 IOINPCALL iocore_inp8(UINT port) {

	if (hootrip_inp8) {
		return (REG8)hootrip_inp8((unsigned)port);
	}
	return 0xff;
}

void IOOUTCALL iocore_out16(UINT port, REG16 dat) {

	if (hootrip_out16) {
		hootrip_out16((unsigned)port, (unsigned short)dat);
	}
	else if (hootrip_out8) {
		/* decompose to byte lanes if only an 8-bit handler is present */
		hootrip_out8((unsigned)port,       (unsigned char)dat);
		hootrip_out8((unsigned)(port + 1), (unsigned char)(dat >> 8));
	}
}

REG16 IOINPCALL iocore_inp16(UINT port) {

	if (hootrip_inp16) {
		return (REG16)hootrip_inp16((unsigned)port);
	}
	if (hootrip_inp8) {
		REG16 v = (REG16)hootrip_inp8((unsigned)port);
		v |= (REG16)((REG16)hootrip_inp8((unsigned)(port + 1)) << 8);
		return v;
	}
	return 0xffff;
}

void IOOUTCALL iocore_out32(UINT port, UINT32 dat) {

	if (hootrip_out32) {
		hootrip_out32((unsigned)port, (unsigned int)dat);
	}
	else {
		iocore_out16(port,     (REG16)dat);
		iocore_out16(port + 2, (REG16)(dat >> 16));
	}
}

UINT32 IOINPCALL iocore_inp32(UINT port) {

	if (hootrip_inp32) {
		return (UINT32)hootrip_inp32((unsigned)port);
	}
	{
		UINT32 v = iocore_inp16(port);
		v |= (UINT32)iocore_inp16(port + 2) << 16;
		return v;
	}
}


/* ---- machine-subsystem stubs ---- */

HOOTRIP_DMAC dmac;                 /* zero-initialised: dmac.working == 0 */

void dmax86(void) {
}

void dmav30(void) {
}

UINT MEMCALL biosfunc(UINT32 adrs) {

	UNUSED(adrs);
	return 0;
}
