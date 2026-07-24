/*
 * bios/bios.h -- HOOTRIP stub
 *
 * i286c_mn.c and v30patch.c #include <bios/bios.h> and call biosfunc() from
 * the NOP (0x90) handler when execution lands in the PC-98 BIOS ROM window
 * (0xF8000-0xFFFFF). A vendored real-mode DOS driver never enters that
 * window, so hootrip's glue provides a no-op biosfunc() returning 0.
 */
#ifndef HOOTRIP_BIOS_H
#define HOOTRIP_BIOS_H

#include <np2types.h>

#ifdef __cplusplus
extern "C" {
#endif

UINT MEMCALL biosfunc(UINT32 adrs);

#ifdef __cplusplus
}
#endif

#endif /* HOOTRIP_BIOS_H */
