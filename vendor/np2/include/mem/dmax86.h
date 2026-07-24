/*
 * mem/dmax86.h -- HOOTRIP stub
 *
 * Mirrors np2kai/mem/dmax86.h: declares dmax86(). The i286c run loop calls
 * dmax86() after each instruction to service DMA; hootrip's glue provides a
 * no-op definition (no DMA controller emulated for the sound-driver use case).
 */
#ifndef HOOTRIP_DMAX86_H
#define HOOTRIP_DMAX86_H

#ifdef __cplusplus
extern "C" {
#endif

void dmax86(void);

#ifdef __cplusplus
}
#endif

#endif /* HOOTRIP_DMAX86_H */
