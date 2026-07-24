/*
 * steptest.c -- HOOTRIP acceptance test
 *
 * Proves the vendored i286c core executes real real-mode x86 and that the
 * flat memory glue round-trips. Builds into a standalone executable; exits
 * 0 on success, 1 on any mismatch.
 *
 * Program loaded at CS=0x1000 (linear 0x10000):
 *     B8 34 12     MOV AX,0x1234
 *     89 C3        MOV BX,AX
 *     01 D8        ADD AX,BX          ; AX = 0x2468, BX = 0x1234
 *     C1 EB 04     SHR BX,4           ; BX = 0x0123   (286-only imm-count shift)
 *     F4           HLT
 */

#include <compiler.h>
#include <cpucore.h>

int main(void) {

	static const UINT8 prog[] = {
		0xB8, 0x34, 0x12,   /* mov ax,0x1234 */
		0x89, 0xC3,         /* mov bx,ax     */
		0x01, 0xD8,         /* add ax,bx     */
		0xC1, 0xEB, 0x04,   /* shr bx,4      */
		0xF4                /* hlt           */
	};
	UINT   i;
	int    failures = 0;

	/* ---- bring the core up ---- */
	i286c_initialize();
	i286c_reset();
	i286c_setextsize(0);

	/* ---- point CS:IP at our program, set a sane stack, real-mode mask ---- */
	i286core.s.r.w.cs = 0x1000;
	i286core.s.cs_base = 0x10000;
	i286core.s.r.w.ip = 0x0000;

	i286core.s.r.w.ss = 0x2000;
	i286core.s.ss_base = 0x20000;
	i286core.s.ss_fix  = 0x20000;
	i286core.s.r.w.sp = 0xFFFE;

	i286core.s.r.w.ds = 0x0000; i286core.s.ds_base = 0x00000; i286core.s.ds_fix = 0x00000;
	i286core.s.r.w.es = 0x0000; i286core.s.es_base = 0x00000;

	i286core.s.adrsmask = 0x000FFFFF;   /* 8086/real-mode 20-bit address bus */

	/* ---- load program bytes at linear 0x10000 ---- */
	for (i = 0; i < (UINT)sizeof(prog); i++) {
		mem[0x10000 + i] = prog[i];
	}

	printf("== i286c step test ==\n");
	printf("start  AX=%04X BX=%04X IP=%04X\n",
	    i286core.s.r.w.ax, i286core.s.r.w.bx, i286core.s.r.w.ip);

	/* ---- single-step the four data instructions ---- */
	i286c_step();   /* mov ax,0x1234 */
	printf("after MOV AX,imm : AX=%04X IP=%04X\n", i286core.s.r.w.ax, i286core.s.r.w.ip);

	i286c_step();   /* mov bx,ax */
	printf("after MOV BX,AX  : BX=%04X IP=%04X\n", i286core.s.r.w.bx, i286core.s.r.w.ip);

	i286c_step();   /* add ax,bx */
	printf("after ADD AX,BX  : AX=%04X BX=%04X IP=%04X\n",
	    i286core.s.r.w.ax, i286core.s.r.w.bx, i286core.s.r.w.ip);

	i286c_step();   /* shr bx,4 */
	printf("after SHR BX,4   : BX=%04X IP=%04X\n", i286core.s.r.w.bx, i286core.s.r.w.ip);

	/* next opcode should be HLT (0xF4) */
	printf("next opcode      : %02X (expect F4=HLT)\n",
	    mem[i286core.s.cs_base + i286core.s.r.w.ip]);

	/* ---- checks ---- */
	if (i286core.s.r.w.ax != 0x2468) {
		printf("FAIL: AX=%04X expected 2468\n", i286core.s.r.w.ax); failures++;
	}
	if (i286core.s.r.w.bx != 0x0123) {
		printf("FAIL: BX=%04X expected 0123\n", i286core.s.r.w.bx); failures++;
	}

	/* ---- memory round-trip through the physical accessors ---- */
	memp_write16(0x00500, 0xBEEF);
	{
		REG16 rd = memp_read16(0x00500);
		printf("memp_write16/read16 @0x500 = %04X (expect BEEF)\n", rd);
		if (rd != 0xBEEF) { printf("FAIL: mem round-trip\n"); failures++; }
		/* verify little-endian byte layout landed in mem[] */
		if (mem[0x500] != 0xEF || mem[0x501] != 0xBE) {
			printf("FAIL: LE layout mem[0x500]=%02X mem[0x501]=%02X\n",
			    mem[0x500], mem[0x501]); failures++;
		}
	}

	/* ---- segment:offset round-trip through memr_* ---- */
	memr_write16(0x0040, 0x0010, 0xCAFE);   /* linear 0x410 */
	{
		REG16 rd = memr_read16(0x0040, 0x0010);
		printf("memr_write16/read16 40:0010 = %04X (expect CAFE)\n", rd);
		if (rd != 0xCAFE || memp_read16(0x410) != 0xCAFE) {
			printf("FAIL: memr round-trip\n"); failures++;
		}
	}

	if (failures == 0) {
		printf("\nPASS: AX=2468 BX=0123, memory round-trips OK\n");
		return 0;
	}
	printf("\n%d FAILURE(S)\n", failures);
	return 1;
}
