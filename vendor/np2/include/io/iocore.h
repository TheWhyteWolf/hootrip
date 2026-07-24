/*
 * io/iocore.h -- HOOTRIP stub
 *
 * Minimal replacement for np2kai/io/iocore.h. The i286c core needs only:
 *   - the port I/O entry points iocore_out8/16/32 and iocore_inp8/16/32
 *     (implemented in glue/np2io.c, forwarding to overridable hooks), and
 *   - the global `dmac` with a `.working` byte, tested by the run loop to
 *     decide whether to take the DMA-servicing path.
 *
 * The real header defines a large `_DMAC` struct in <io/dmac.h>; the core
 * only ever reads `dmac.working`, so a one-field struct is layout-sufficient.
 */
#ifndef HOOTRIP_IOCORE_H
#define HOOTRIP_IOCORE_H

#include <np2types.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Only member the i286c/v30 core touches: dmac.working */
typedef struct {
	UINT8 working;
} HOOTRIP_DMAC;

extern HOOTRIP_DMAC dmac;

/*
 * PICEXISTINTR -- "is a hardware IRQ pending?" Upstream this comes from
 * <io/pic.h> and tests the 8259 IRR/IMR. hootrip emulates no PIC, so no
 * external interrupt is ever pending: it is a constant 0. Used by the
 * i286c POPF/IRET/STI handlers to decide whether to break the run loop for
 * interrupt servicing (with 0 they simply never break for that reason).
 */
#ifndef PICEXISTINTR
#define PICEXISTINTR (0)
#endif

void   IOOUTCALL iocore_out8(UINT port, REG8 dat);
REG8   IOINPCALL iocore_inp8(UINT port);
void   IOOUTCALL iocore_out16(UINT port, REG16 dat);
REG16  IOINPCALL iocore_inp16(UINT port);
void   IOOUTCALL iocore_out32(UINT port, UINT32 dat);
UINT32 IOINPCALL iocore_inp32(UINT port);

#ifdef __cplusplus
}
#endif

#endif /* HOOTRIP_IOCORE_H */
