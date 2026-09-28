---
title: PC-98 driver families
---

# PC-98 driver families — how each one is re-hosted

This document records, per sound-driver family, how hoot's PC-98 (`pc98dos`) sets
deliver a song to their driver and how the hootrip harness reproduces that so the
driver plays and we can log its OPN/OPNA register writes to S98/VGM.

It focuses on the families brought up in the 2026-07-24 "remaining families"
campaign, with enough of the shared model to make the fixes legible.

---

## 1. The shared model

A `pc98dos` set is a real MS-DOS re-host, not a memory blob:

1. **Materialize** every `<rom>` file into a virtual DOS CWD (real `.COM`/`.EXE`/
   data files live there).
2. **Bind rom → DOS handle**: hoot presents each `<rom offset="N">FILE</rom>` on
   DOS file handle `N`. In the harness (`bind_rom_handles`):
   - `type="file"` → the file's **content** is readable on handle `N`.
   - `type="conin"` → the **filename text** (ASCIIZ basename) is on handle `N`.
   - `offset="-1"` → materialized on disk only (open-by-name works), not handle-bound.
3. **Load device drivers** (`<rom type="device">*.SYS/.DRV/.EXE`) CONFIG.SYS-style:
   run their INIT so they install their API interrupt.
4. **Run the shell chain** (`<rom type="shell">` in order). The last command is
   usually a tiny hoot glue stub (`xxxx_98.COM`) that installs one of hoot's
   *externalCommand* vectors, **INT 7Eh or INT 7Fh**, and then idles.
5. **Trigger playback**: the harness sets virtual ports and invokes the stub's
   INT 7Eh/7Fh. The stub loads the selected song and starts the driver, whose
   **timer ISR** (OPN chip timer on IRQ3 = INT 0Bh, the PC-98 PIT on INT 08h, a
   slave IRQ like INT 14h, or the CRT VSYNC on IRQ2 = INT 0Ah) then sequences the
   music. We record every OPN register write during this phase.

### Virtual trigger ports (modelled in `pc98/io.rs`)

| Port  | Name        | Meaning |
|-------|-------------|---------|
| 0x7E0 | `EXT_CMD`   | command byte: 0 = play, 2 = stop |
| 0x7E2 | `EXT_SONG`  | song word the harness sets before the trigger |
| 0x7E4 | `EXT_PARAM` | extra selection parameter |
| 0x7E8 | `EXT_STATE` | stub handshake (`0x81` = ready) |

### Song selection (`selected_song`) and delivery (`bind_trigger_song`)

`selected_song(title_code)` picks the song file by matching `offset` against the
title code — a `file` rom (whole code, then low byte), then a **`conin` rom at the
low byte** (excluding engine names ending `.EXE/.COM/.DRV/.SYS`, so a low byte
that collides with an engine handle can never select the engine as the song).

`bind_trigger_song` then presents it in the convention the driver expects. The
central discovery of this campaign is that **three unrelated drivers share one
bug**: they open the song *file by name themselves*, but hoot's stubs were handing
them the file *content*. They are handled by the `opens_by_name` branch, which
puts the **filename text** (not content) on handle 0.

---

## 2. Families

### cplay98 (~87 games) — Madou Monogatari, Puyo Puyo, …
- **Chain**: `fplay2` (driver `FPLAY2.COM`) + `cplay98` (stub, INT 7Eh).
- **Songs**: multi-song `.dat` banks (`Song1.dat`…), listed as `conin` roms at
  offsets 5–9. Title code `0xSS00LL`: low byte `LL` picks the bank, byte 2 `SS` is
  the 0-based in-bank song index.
- **How it plays**: the stub reads a filename from handle 0 and calls FPLAY2's
  `INT 7Fh AH=9`, which does `int21 AX=3D00` (**open by name**), reads and parses
  the whole `.dat` itself, then `AH=0 AL=SS` plays the in-bank index.
- **Bug**: `selected_song` only matched `file` roms, so the `conin` song banks
  returned nothing → handle 0 empty → the driver opened `""` → nothing loaded.
- **Fix**: `conin`-rom fallback in `selected_song`; `opens_by_name` puts the
  **filename** on handle 0; port 0x7E4 = byte 2 (the in-bank index).
- **Result**: 7/8 sampled OPN games audible (−4…−12 dBFS); BEEP variants correctly
  silent.

### artdi_98 (~52 games) — A-Train, Atlas II, Lunatic Dawn, …
Combined driver+stub `ARTDI_98.COM` (byte-identical across the family) overlay-
loads a per-game engine EXE (named on `conin` handle 5, AH=4B03) and hooks INT 7Fh.
Two song layouts:

- **Separate `.NTL` files** (A-Train): one file per song at offsets 0x10+; title
  low byte = the file offset. The engine is **VSYNC-paced** — it hooks INT 0Ah
  (IRQ2), unmasks only IRQ2, and both calibrates on and sequences off the CRT
  vertical retrace. It hung in setup because the harness had no VSYNC source.
  - **Fix**: added a general ~60 Hz **VSYNC/IRQ2 → INT 0Ah** source (see §3).
- **Packed `NTL.PAC`** (Atlas II, Lunatic Dawn, How Many Robot 2): all songs in
  one pack bound on a handle; title `0x[HH][SS]` = pack handle `HH` + in-pack index
  `SS`. ARTDI's INT 7Fh handler takes a **packed branch when the high byte of port
  0x7E2 is nonzero**: it seeks that handle and indexes `SS` itself via the pack's
  leading LE32 size table (`offset(SS) = N*4 + Σ sizes[0..SS)`).
  - **Fix**: `bind_trigger_song` self-identifies a packed title (a `.PAC` `file`
    rom at offset = title byte 1) and presents the full `0x[HH][SS]` word on 0x7E2.
- **Result**: 8/8 sampled audible; both layouts covered; A-Train unchanged.

### mbmusp (~19 games) — Estate, Ce'st la vie, Coming Heart, …
- **Chain**: `MUSDRV` ("Music driver 1.01b, YM2608") + `MBMUSP` (stub, INT 7Fh).
- **How it plays**: `INT 68h AH=1` = **open song file by name**, read, **deobfuscate**
  (bswap/rol/not/ror — a raw buffer cannot be played), then start OPN Timer A and
  self-clock. The stub hands AH=1 a memory buffer, so its open failed → the timer
  was never started (song began then froze after ~0.35 s).
- **Two-part fix**:
  1. **Open-by-name**: filename text on handle 0 (the stub copies it into its
     buffer and passes it, so the open now finds the materialized `.MSB`).
  2. **IRQ vector jumper**: MUSDRV picks its ISR vector from OPN SSG **reg 0x0E**
     bits 7-6; the default `0x00` makes it hook INT 0Bh (master IRQ3), but its ISR
     EOIs/unmasks the *slave* PIC — so the master IRQ3 stayed masked and 0 IRQs
     were delivered. Presetting reg 0x0E = 0xC0 makes it hook **INT 14h** (slave
     IRQ12), consistent with its own EOI. (`preset_muse_irq_jumper`.)
- **Result**: 7/8 sampled audible (−6…−22 dBFS).

### mdrv_98 acidplan family (~10+ games) — Charm, Present Duo, Metal Mover, …
- **Chain**: `MDRV` (driver `MDRV.COM`) + a `-t<timbre>.TDT` load + `MDRV_98`/
  `MDDRV_98` stub.
- **Hypothesis inverted**: residency detection and the `-t` timbre load *both work*
  in sequence (a `--trace` in isolation was misleading, and exit 255 for a resident
  `-t` option is normal; the driver also ships an embedded default timbre bank).
  The real bug was again **open-by-name**: resident `INT D2h AL=2` = "load song file
  by name"; the stub passed the KMD content, the open failed, and AL=1 parsed an
  empty buffer (4 key-ons then silence).
- **Fix**: add the stub prefix `mdrv_9`/`mddrv_9` to `opens_by_name` → filename on
  handle 0. **Scoped deliberately to the underscore stub**, not `mdrv98`, to leave
  the separate MDRV98+mlp_hoot family (below) on its content path.
- **Result**: 6/8 sampled audible.

### MDR external-voice (7 sets) — Zeta, Ginga Tetsudou, J.League, …
- `MDR.EXE` is a DOS char device. The **small build** (~7.5 KB) keeps FM timbres in
  external `.VOI` files; the **big build** (~25 KB, Wolf Pack) embeds them.
- Title `0x[VH]00[SH]`: byte 2 `VH` = the DOS handle of the `.VOI` timbre bank, low
  byte `SH` = the song `.SEQ` handle. The stub reads the voice handle number from
  **port 0x7E4**, loads that handle into the driver (INT 2Fh/40h AX=879B BX=1/2),
  then plays the song from handle 0. Without the voice the FM operators are never
  programmed → notes at reset attenuation → silent.
- **Fix**: `bind_trigger_song` self-identifies (a `.VOI` rom at offset = byte 2) and
  sets port 0x7E4 = byte 2. Wolf Pack (embedded, byte 2 = 0) is untouched.
- **Result**: the external-voice sets render (Zeta −18, Ginga Tetsudou −21).

### MUSE device family (~10 games) — NMUSE / MUSE2 / MUSE3 / SDD_2
- `.DRV` char devices + `muse_98.com` stub. The stub locates the resident driver via
  the **segment word of INT 14h** (`0000:0052`); the drivers choose their vector from
  OPN SSG **reg 0x0E** and, at the default `0x00` readback, hook INT 0Bh/15h — never
  INT 14h — so the stub reads a bogus segment, wild-jumps, and never installs.
- **Fix**: in `load_device_drivers`, when the loaded device header name contains
  `MUSE`/`SDD`, preset reg 0x0E = 0xC0 before INIT so all three hook INT 14h.
- **Result**: **NMUSE works** (Rakuichi −6.6). **MUSE2/MUSE3/SDD still spin** in the
  stub after the vector is fixed — an additional, unresolved blocker (~6 games).
- Note: the ASCII `muse.com` variant (bpbb98) is a *different* driver that self-loads
  `MUSE.DRV` from handle 5 and already worked; no change needed.

### MDRV98 + mlp_hoot family — Markadia, Twins, Camel-Zoo, Bakutotsu, Owl-Zoo
- `MDRV98.COM` ("MDRV2 ver 3.4E") + `mlp_hoot` stub (INT 7Fh → reads the song
  **content** from handle 0 → drives MDRV2 via **INT F2h**). This uses the *content*
  path, not open-by-name.
- **Not broken.** It renders real music at every real song index (OPN and OPNA,
  −10…−18 dBFS). The earlier "captured-but-silent" report was a **sweep artifact**:
  title index 0 is a `演奏停止` (playback-stop) pseudo-track, silent by design. The
  only care needed was to keep the mdrv fix off this family (the `mdrv_9` scoping).

---

## 3. General levers (help beyond one family)

- **VSYNC / IRQ2 → INT 0Ah source** (`io.rs` `tick_vsync` + `harness.rs`
  `deliver_timer_irq`): a ~60 Hz retrace latch delivered as INT 0Ah **only when a
  driver has hooked INT 0Ah and unmasked IRQ2** at the master PIC. Frame-paced
  engines (artdi separate-file, and the infra "Family B"/VSYNCMAN needs) advance one
  tick per frame; the `out 0x64` VSYNC ack clears the latch. Drivers that pace on
  IRQ0/IRQ3 leave IRQ2 masked and are unaffected.
- **`selected_song` conin-song fallback** with an `.EXE/.COM/.DRV/.SYS` engine-guard.
- **Open-by-name delivery** (filename text on handle 0) — one branch now serving
  cplay98 (FPLAY2/AH=9), mbmusp (MUSDRV/AH=1), and mdrv_98 (MDRV/AL=2).
- **OPN SSG reg-0x0E board jumper** preset — makes MUSE and mbmusp select the IRQ
  vector consistent with their own EOI code.

---

## 4. Measurement caveat — the index-0 stop-track

A smoke sweep that rips **title index 0** false-negatives **146 pc98dos games**
whose first title is a stop/SE pseudo-track (`[STOP]`, `演奏停止`, `無音`, `SND OFF`),
which are silent by design. Real coverage is materially higher than an index-0
sweep shows. Use `famtally.py`, which selects the first non-stop title, or rip all
titles and treat a designed-silent track as expected. The Markadia "regression"
was entirely this artifact.

---

## 5. Verification

- Every fix is verified by **rendering** (libvgm `vgm2wav`, peak dBFS), never by
  write count alone (a driver can log plausible-but-silent registers).
- A 14-family known-good baseline (`baseline_ref.txt`) is re-run after each change;
  all fixes above landed with **0 regressions** and the full test suite green.

## 6. Still open (per-family RE remaining)
- **MUSE2 / MUSE3 / SDD_2** (~6 games): stub spins after the vector jumper.
- **Charm 2's MDDRV_98** variant: some indices deliver 0 IRQs (timer/vector quirk).
- **Family B "Night Seep"** (VSYNCMAN + SNDDRV2): the VSYNC infra is in place, but
  VSYNCMAN's frame-callback chain and song delivery need wiring.

---

## 7. The remaining silent sets — a ranked queue

After the audibility gate landed (see [silent-rips](silent-rips.html)), the sets
that produce *nothing* are countable rather than hidden in a pile of silent
files. Measured over a full rip of HootArchive20240621:

**393 OPN-family sets are entirely silent, holding 8,766 titles** — 310 pc98dos
sets (6,676 titles) and 83 pc88 (2,090). MIDI/GS/MT-32 variants are excluded;
those are silent by construction and are not failures.

Grouped by the **last shell command** — the hoot glue stub that installs the
INT 7Eh/7Fh trigger — the pc98dos half is a long tail of driver families, not
one bug:

| titles | sets | stub |
|---:|---:|---|
| 808 | 29 | `music_98` |
| 480 | 28 | `fgplay_h` |
| 474 | 13 | `pmd_98` |
| 398 | 11 | `valky_98` |
| 230 | 11 | `nlp_hoot` |
| 218 | 12 | `emd_98` |
| 216 | 14 | `fmxp` |
| 208 | 7 | `usmd` |
| 208 | 12 | `usd_98` |
| 185 | 12 | `magic_98` |
| 156 | 10 | `cplay98` |
| 151 | 4 | `odq_98` |

`pmd_98` and `cplay98` appearing here is worth noting: both families are
supported, so those are per-set failures within a working family rather than a
missing family.

### What `music_98` does (the largest cluster)

Chain is `MUSIC.COM -r` (driver, goes resident) then `music_98.com` (hoot's
stub, installs INT 7Fh). Songs are **MML source**, bound as `type="file"` roms on
DOS handles `0x0b`–`0x1b`, and the title code is the handle number — so
`MUSIC.COM` is compiling MML at run time rather than loading compiled data.

Tracing `bakasuka_98` (34 titles) shows the driver reaching a healthy resident
state and then failing to play:

- console confirms residency (`音楽ドライバー Ver 1.00 … メモリーに常駐しました`)
- INT 7Fh is installed by the stub (`INT 0x7f -> 0x2002:0x014c`)
- the OPN timer runs — 259 timer IRQs over the capture
- but only **3 key-ons**, and every register write falls inside the first
  **0.711s of a 5s capture**

**Resolved.** The driver was fine; the harness was delivering its interrupts to
the wrong vector.

`music_98` puts its sequencer ISR on **INT 14h** (IRQ12 — the PC-98 sound
board's interrupt jumper can route the OPN `/IRQ` there instead of IRQ3, and
this harness already forces the jumper bits that select it). But it *also* hooks
INT 0Ah, and `opn_sound_vec()` fell through to its positional fallback — "lowest
hooked hardware vector" — which picked 0x0A. Every OPN timer IRQ was delivered
there, landed on the DOS trampoline with no handler, and the sequencer never
advanced.

`0x14` now sits alongside `0x0B` as a vector a driver names outright, preferred
over the positional guess. On `bakasuka_98`:

| | before | after |
|---|---|---|
| sound vector | 0x0A | **0x14** |
| timer IRQs | 259 | 561 |
| FM writes | 368 | 2,869 |
| key-ons | 3 | **149** |
| write span | 0.000–0.711s | **0.000–4.969s** of 5s |

The set rips 34/34 audible, all distinct, from zero.

**Across the corpus:** re-ripping the 295 previously-silent pc98dos archives
recovers **57 sets and 1,634 tracks**. A control of 72 sets that already ripped
cleanly shows **zero regressions** (70 identical, 2 improved).

The paragraphs below record how the failure presented, since the same shape —
driver resident and ticking, but almost no key-ons and a write span far shorter
than the capture — is the signature of a misrouted sound vector.


**The obvious hypothesis was wrong,** and ruling it out is what pointed at the
vector. `clockmul = 8` means an MML compile costs
real emulated time, so the trigger firing before compilation finishes looked
likely. It is not: raising `--setup-seconds` from 3 to 10 to 25 changes nothing
at all — 368 FM writes, 103 captured, 3 key-ons, span 0.000–0.711s, byte for
byte identical every time. Whatever stops this family is deterministic and
happens early, not a race with setup.

That points at the trigger itself rather than timing. Songs here are bound as
`file` roms on handles `0x0b`–`0x1b` and the title code *is* the handle number,
which is unlike the families in §2 where the song is presented on handle 0. The
next step is to disassemble `music_98.com`'s INT 7Fh handler and establish what
it actually reads — which port it takes the song number from, and which handle it
opens — rather than assuming it follows the handle-0 convention.

The diagnostic to start from:

```sh
hootrip --archive <archive> pc98 "Bakasuka Wars (OPN)" --index 0
```

which prints the interrupt vector table, timer state, write span, key-on count,
the MCB chain, unimplemented DOS/INT calls, and the guest's console output.

---

## 8. Plan for the sets that are still silent

After the INT 14h fix, **75 sets / 2,084 tracks** recovered and **268 sets /
5,518 titles** remain silent (MIDI/GS variants excluded — those are not
failures). Running the `pc98` diagnostic on a representative of each large
cluster sorts them into **five failure signatures**, and the signature, not the
driver name, is what determines the work.

Beware when building these lists: `--only-archives` selects by archive folder,
and the OPN and GS variants of a game share one. An unfiltered list pulls in GS
sets that were never candidates and makes the remaining pile look larger and
more driver-diverse than it is.

### A. Unimplemented driver API interrupt — ~710 titles

The stub installs, then the driver calls an interrupt the harness does not
service and either spins or gives up. Nothing is hooked, no timer runs, no
register is written.

| cluster | titles | evidence |
|---|---:|---|
| `fgplay_h` | 480 | `INT 0xd2 AH=0x00` **×1,142,859** — a spin |
| `nlp_hoot` | 230 | `INT 0x60 AH=0x01/0x02`, plus DOS `AH=47h` |

Most tractable of the five: the call is named, the count is unambiguous, and
INT D2h already has a partial implementation for the MDRV family. Start here.

### B. Pacing source hooked but not running — ~398 titles

`valky_98` hooks INT 08h (PIT), 0x0B, 0x50 and 0xB0, but `opn timer used:
false` and exactly **1** timer IRQ arrives. The driver is PIT-paced and the PIT
is not ticking, so the sequencer advances once and stops. Look at PIT
programming and the IRQ0 unmask path rather than at the driver.

### C. Activity falls outside the capture window — ~218 titles

`emd_98` is alive: **479 timer IRQs and 842 FM writes**. But only **1 write is
captured**, the span is 0.000–0.010s of a 5s capture, and there are no key-ons.
The driver ran during setup and was finished before recording began. This is a
trigger/capture-ordering question, not an emulation gap.

### D. Voice data never loaded — ~426 titles

`pmd_98` (the 10 sets the INT 14h change did not fix) reaches **1,408 timer
IRQs, 33 key-ons and a 4.693s write span** — it is sequencing real music — yet
classifies as `NoVoice`, meaning no operator TL was ever programmed below
maximum attenuation. Notes are being played on instruments that were never
defined. The timbre/voice file is not reaching the driver.

That this family is *partially* fixed matters: the same stub works elsewhere, so
the difference is per-set, most likely in how the voice file is bound.

### E. Everything else — the tail

`usmd`, `usd_98`, `magic_98`, `cplay98`, `odq_98` and friends, 150–210 titles
each. Worth re-running the diagnostic across all of them and bucketing by
signature before touching any code — on this evidence the buckets will not
follow the driver names.

### Suggested order

1. **A** (~710 titles) — named missing calls, clearest fix.
2. **D** (~426) — a working family failing on a subset; the delta should be findable.
3. **C** (~218) — ordering, likely cheap once understood.
4. **B** (~398) — PIT work, more invasive.
5. **E** — re-diagnose and re-bucket first.

### The diagnostic

```sh
hootrip --archive <archive> pc98 "<game>" --index 0
```

Read in this order: which vectors got hooked, `sound vector`, `timer IRQs`,
`FM writes` (total vs captured), `key on/off`, `write span`, then the
unimplemented-call tally and the guest's console output. The five signatures
above are each visible in those lines alone.


---

## 9. Signature A, worked through

Signature A was "the driver calls an interrupt the harness does not service".
That held, but the *interrupt* turned out to be a symptom in every case, not the
cause — and the two clusters had nothing in common beyond the symptom. Three
separate defects, all in the DOS/BIOS re-host rather than in any driver family:

| defect | what it broke | sets | titles |
|---|---|---:|---:|
| no environment block (`PSP:0x2C = 0`) | FUGA OPNDRV 2.04+ overwrote its own code | 27 | 418 |
| PC-98 timer BIOS (INT 1Ch) unserviced | Packen NL / MUAPLAY calibration never ended | 9 | 167 |
| no sound-BIOS ROM (`dummysndrom`) | FUGA OPNDRV 1.23 and others found no board | 4 | 125 |

Two sets are left in A, both **OPNA variants whose OPN twin already rips**:
`sakura_k_98` and `majokko_98` run NAX 6.26, whose OPNA path is **80386 code**
(`push edx`, `push fs` at NAX.COM:12A9). The i286 core traps it as an invalid
opcode ~2M times. Fixing that means an i386 core, not a driver change.

### A.1 — The environment block

`fgplay_h` presented as `INT 0xd2 AH=0x00 ×1,142,859`: a spin. The spin is
`fgplay_h.com` at 0x14A, polling the driver's status call until it reports
ready, which can never happen because the driver is not resident. The stub was
never the problem.

`OPNDRV.COM` **2.04 and later** copy their own program path out of the
environment block and into their PSP, so it survives the block being freed:

```asm
mov  ah,0x30 / int 0x21   ; DOS 3+?
mov  bx,[0x2c]            ; the environment segment
dec  bx / mov es,bx       ; its MCB
mov  cx,[es:3] / shl cx,4 ; block size in bytes
repne scasb / dec cx / scasb / jnz  ; find the \0\0 ending the variable list
rep  movsb                ; copy what follows into the PSP
```

With `PSP:0x2C = 0` — which is what `load_com` wrote, `// no environment for
now` — `dec bx` makes `ES = 0xFFFF`, the size comes from `[0xFFFF:0003]`, and
`rep movsb` copies ~9,500 bytes of low memory over the program's own code.
OPNDRV then falls into the PSP's `INT 20h` and terminates, which the diagnostic
reported as `opndrv -> Terminated(0)` with no hooked `INT D2h`.

Versions **2.03 and earlier do not have this code**, which is exactly why 5 of
the 32 `fgplay_h` sets always worked and 27 never did. Version, not game:

| OPNDRV | before | after |
|---|---|---|
| 1.23, 1.32, 2.00, 2.02, 2.03 | Resident | Resident (unchanged) |
| 2.04, 2.05, 2.06 | **Terminated(0)** | **Resident** |

`MiniDos::alloc_env` now builds the real thing — variable strings, the NUL that
closes the list, a `0x0001` count word, the program's path — allocates it below
the PSP as DOS does, and stamps the PSP as its owner so `AH=49h` can free it.
Rendered through `vgm2wav`, 20 tracks sampled across 5 of the recovered sets
peak at −6 to −19 dBFS; none is silent.

### A.2 — The PC-98 timer BIOS

Packen Software's NL 1.32 / MUAPLAY 1.21 / NAX size their I/O busy-waits by
measuring the machine: arm a one-shot callback, count iterations of a tight loop
until it fires, keep the count.

```asm
mov ah,2 / mov cx,2 / mov bx,0x184d / push cs / pop es / int 0x1c
inc word [0x2124] / mov cx,0x10 / loop $      ; count
cmp byte [0x2126],0 / jz  ...                 ; until the callback sets the flag
```

`INT 1Ch` is the PC-98 **timer BIOS**, and it was unserviced, so the flag never
set and the driver counted forever — `nl -> RanToBudget`, nothing installed.

Two things were needed. `MiniDos::int1c` records the request, and the harness
turns it into a cycle deadline and enters the routine as an interrupt (`enter_far`
— `X86Cpu::interrupt` only dispatches through the IVT, and a BIOS callback has no
vector). And the call must **return with interrupts enabled**: the driver arms the
one-shot with `IF` clear, so a BIOS that returned as it was called could never
deliver what it had just armed. `enable_irqs_on_return` sets `IF` in the frame the
trampoline's `IRET` restores.

The tick is modelled at 100 Hz. Only its order of magnitude reaches the music —
the constant sizes busy-waits; tempo comes off the OPN timer or the PIT.

All 9 sets install and play: `nl` ×5 (Spread, Outer Formula, CRW Metal Jacket,
Magic Master, Block Quest V) and `MUAPLAY` ×4 (Kids SAP, Quintia Road, Wrestle
Angels, Presence), 45–239 key-ons each over a 3.2–5.0 s window.

### A.3 — `dummysndrom`

A real PC-9801-26K/86 board carries a BIOS ROM, and software that drives the
board through it asks the ROM which software interrupt it serves. hoot maps a
stand-in for the 76 sets that ask (`<option name="dummysndrom" value="1"/>`);
the harness mapped nothing, so OPNDRV 1.23 read zero, concluded there was no
sound board, and kept an 80-byte stub resident instead of the driver:

```asm
mov ax,0xcee0 / mov es,ax / mov al,[es:4]   ; the sound BIOS's interrupt number
mov [0x142],al
cmp byte [0x142],0xd2 / jnz <no board>
```

We model that one byte, the only field we have direct evidence a driver reads,
rather than inventing ROM contents — a set that probes something else stays
visibly silent instead of quietly wrong. It recovers Beat Vice, Imadoki Junjyou
Monogatari, Zark Legend Special and Amida Extra (125 titles). Yoshitsune gets as
far as 1,429 timer IRQs and a full-length write span but still no key-on, and
the `odq_98` sets (Deflektor, Majoriko, H-Go! Yeah!) are unchanged — those fail
for other reasons.

### A.4 — Validation

Ripped in full and rendered through `vgm2wav`: **261 of 263 tracks across the 13
sets A.2 and A.3 recovered are audible**, −1.8 to −21.6 dBFS. The two exceptions
are `音色定義` timbre-definition pseudo-tracks (Outer Formula `NEIRO_2.O`, CRW
Metal Jacket `MJ_00.O`) — 95 writes, 60-odd registers programmed, nothing keyed
on, silent by design. The gate calls them `dead` and drops them; finding them in
the output is what surfaced the `pc98-rip` gate gap recorded in
[silent-rips](silent-rips.html) §2.

For the `fgplay_h` sets, 20 tracks sampled across 5 of the 27 peak at −6 to
−19 dBFS with none silent.

**Control: 70 previously-working sets, one from each of ~50 stub families, all
14 titles or fewer, re-ripped in full and compared against the promoted library
on the dump-region identity `hootrip triage` records.** 688 tracks, 100%
audible, **609 byte-identical**. The 79 that differ fall in 7 sets:

| set | delta | what changed |
|---|---:|---|
| AD&D Dragon Strike (OPN) | +0.063% | the intended `dummysndrom` init (`00 07 bf` now leads the stream) |
| ESP, Ekudorado (86), Kara no Naka no Kotori, Poison Needle (OPNA), Ryuou Sangokushi, Touhou Reiiden (OPNA) | ≤0.005% | one-tick wait rounding |

The rounding is a 1 ms `0xFF`/`0xFE` wait landing on the other side of a
register write — `00 28 f5 FF 00 a4 0c` where the library has
`00 28 f5 00 a4 0c FF`, or `FE 0a FE 0b` where it has `FE 0b FE 0a`. Same
events, same order, same total elapsed time; the environment block moves every
PSP three paragraphs, which shifts the setup phase by a few cycles. Durations
are identical and peaks match within 0.4 dB throughout.

**A trap in reading that comparison.** Three library folders hold tracks from
more than one catalogue entry, because distinct archives render to the same
display name: `dang_98` (8 titles) and `dang2_98` (39) both write to
`[PC-9801] Hana Yori Dango 2 (OPN)`, and `fm_variant_game_name` maps a set's
GS/MT-32 entries onto its `(OPN)` folder as well. A control that rips one entry
and diffs the folder reports the other entry's tracks as missing. That is the
79 "missing" in this run, and it is not a rip failure.

---

## 10. Where the queue stands after signature A

Measured 2026-09-16 by re-ripping every pc98dos archive that was entirely
silent in the promoted library (`archive-rip --only-archives`, 15 s captures —
enough to tell audible from silent, not a library rip), then re-running the
recovered subset against the pre-signature-A build to attribute the gains.

**337 OPN-family sets were silent at promotion. 111 now produce audio (2,511
titles); 226 remain (4,731 titles).** Split by cause:

| | sets | titles |
|---|---:|---:|
| the INT 14h sound-vector fix (before today) | 70 | 1,820 |
| signature A (environment block, INT 1Ch, `dummysndrom`) | 41 | 691 |

Signature A's 691 breaks down as `fgplay_h` 28 sets / 470, `nlp_hoot` 9 / 165,
and 4 sets / 56 titles elsewhere that the `dummysndrom` byte unblocked
(`zark_98`, `yositune`, `imado_98`, `amidaex`).

**Do not read the INT 14h fix's reach as signature A's.** That fix alone
accounts for `pmd_98`'s 461 recovered titles, `mxj_98`'s 131 and `fmxp`'s 127 —
families signature A never touched. Comparing a fresh sweep against the
promoted library conflates the two, because the library predates both.

### What remains

| stub | sets | titles | signature |
|---|---:|---:|---|
| `valky_98` | 11 | 398 | B — PIT hooked but not ticking |
| `pmd_98` | 9 | 357 | D — sequencing real music, no voice ever programmed |
| `emd_98` | 12 | 218 | C — activity finishes before the capture opens |
| `fmxp` | 14 | 216 | — |
| `usmd` | 7 | 208 | — |
| `usd_98` | 12 | 208 | — |
| `odq_98` | 5 | 192 | — |
| `magic_98` | 12 | 185 | — |
| `cplay98` | 10 | 156 | — |
| ~20 more | 134 | 2,593 | — |

**§11 supersedes the `pmd_98` and `fmxp` rows.** Both split on the PC-9801-86
board, not on the family: `pmd_98`'s 9 sets and 9 of `fmxp`'s 14 are gone from
this queue. `valky_98` is what the order should follow now.

Two cautions carried forward from today, both now with a second data point:

- **The stub still does not predict the failure.** `fgplay_h` split on the
  *driver version* inside it (OPNDRV ≤2.03 worked, ≥2.04 never did);
  `nlp_hoot` split into three unrelated causes; and `fmxp` and `pmd_98` now sit
  on *both* sides of the line — 9 `fmxp` sets recovered and 14 did not.
  Re-diagnose before grouping.
- **Fixing the re-host beats fixing a family.** Every gain above came from the
  DOS/BIOS layer — a vector, an environment block, a BIOS call, a ROM byte —
  and each one reached sets nobody was aiming at. `cplay98` and `usd_98`
  appearing here at all is the same signal in reverse: supported families
  failing on a subset, which has so far always meant a per-set binding
  difference rather than a missing capability.

---

## 11. The PC-9801-86 board, worked through

Signature D was filed as "voice data never loaded" on the strength of a
`pmd_98` set that sequenced real music and classified `NoVoice`. It was not a
voice-binding problem, and `pmd_98` was not the unit of work. Re-bucketing the
sweep by **machine kind** and by **whether a set's same-archive twin rips**
found the real boundary in one pass:

| still-silent sets | sets | titles |
|---|---:|---:|
| a twin in the same archive folder rips | 24 | 716 |
| …of which kind `86` | 22 | 653 |
| no working twin | 202 | 4,015 |

All nine remaining `pmd_98` sets were the **`(86)`** variant of a set whose
`(OPN)` twin already ripped — `imd_4_98` had a working `(OPNA)` twin too. Same
driver binary, same song data, same harness configuration (kinds `86` and
`opna` are folded together everywhere: same chip, clock, ports and S98 device).
Only the *shell chain* differed, and the 86 chain pulls in the board's PCM
driver. Two defects were hiding behind that, and neither is a driver-API gap.

### 11.1 — The PCM FIFO that never filled

PMD86's IRQ handler polls the 86 board's PCM control register:

```asm
2E53  mov dx,0xa468
      in  al,dx
      test al,0x10     ; bit 4: "the FIFO wants more data"
      jz  0x2e60       ; clear -> go sequence the FM chip
      call 0x677       ; set   -> push another block
      jmp 0x2e53
```

`0xA468` had no read arm, so it returned the unmodelled-port default of `0xFF`
— bit 4 permanently set. The driver refilled a FIFO that never filled and never
reached the FM sequencer: **860,060 reads of one port** in a five-second
capture, 117 FM writes, a 1 ms write span.

The 86 board's PCM is a separate DAC with no S98 or VGM device to carry it, so
the honest model is a FIFO that never starves — `PCM86_FIFO_REQ` always reads
clear. The driver skips its PCM feed and gets on with the FM, which is the part
we can log. Writes to `0xA468` are now retained so the driver's
read-modify-write rate and FIFO-reset updates see their own bits back, and the
rest of `0xA461..0xA46F` reads as 0 rather than falling through to 0xFF — a
driver polling any of them for a flag should see "nothing pending", not "every
bit set". Same set afterwards: 18,848 FM writes, 472 key-ons, the full 5 s.

### 11.2 — A `.COM` that would not fit in 65,408 bytes

Grounseed then failed one step later, on its own stub:

```
pmd_98  -> Error("out of memory loading .COM (1805 bytes)")
```

with 63 KB free. `load_com` asked the arena for a round `0x1000` paragraphs —
its own comment said "allocate the largest block we can", but the code demanded
exactly 64 KB. Grounseed's `P86DRV /24` takes a 384 KB PCM buffer, leaving a
largest free block of `0xFF8` paragraphs: 128 bytes under 64 KB, and ample for
a 1.8 KB stub. DOS hands a `.COM` the largest block it has, so the loader now
does too, and parks SP at the top of what it actually got instead of a presumed
`0xFFFE`. That is a general DOS-layer fix; it happened to surface here because
the 86 chains are the ones that allocate big buffers.

### 11.3 — Result

Re-swept all 337 previously-silent OPN-family sets against the pre-fix build:

**24 sets recovered, 595 of their 742 titles, 0 regressions and 0 title-count
changes anywhere else.**

| stub | sets | titles | |
|---|---:|---:|---|
| `pmd_98` | 9 | 298 | all of signature D's remainder |
| `fmxp` | 9 | 127 | the `(86)` half of the split noted in §10 |
| `klp_hoot` | 2 | 86 | |
| `rhymes98` | 1 | 33 | |
| `fmxpb` | 2 | 26 | |
| `pmp_hoot` | 1 | 25 | |

Two independent checks on the other side of the ledger:

- 41 of 41 sampled recovered tracks render through libvgm at −0.0 to −15.0 dBFS,
  so the audibility call is not just our own classifier agreeing with itself.
- The 70-set control from §9.4 — all audible before this change, one of them an
  `86`-kind set — re-ripped at full length: **688 of 688 tracks byte-identical**.
  Unlike the environment-block work, this change moves nothing in a set that was
  already playing: the new `0xA461..0xA46F` read arms only fire on ports that
  previously counted as unmodelled, and the loader takes the same round `0x1000`
  paragraphs whenever the arena has them.

### 11.4 — What is left of the 86 kind

6 sets / 239 titles, and none of them are 86-board problems — the diagnostic
puts each in a signature that has nothing to do with the board:

| set | titles | signature |
|---|---:|---|
| `valkyrie_98`, `mariner_98`, `injuda_98` | 192 | B — `VALKY_98` runs to budget or exits; PIT not ticking |
| `v_btr_98`, `v_ctr_98` | 35 | FMX/FMXP run to budget |
| `msw98` | 12 | `puzp` runs to budget |

**The queue after this work: 202 sets / 3,989 titles.** `valky_98` (signature B)
is now both the largest single bucket and the only thing standing between us and
the last of the 86 sets, which moves it up the order.

One loose end noted while mapping the board and left alone: a write to `0xA460`
is interpreted as the OPNA "extend" bit, and a driver writing a PCM mode byte
with bit 0 clear would silently disable bank-1 readback. No set is known to do
it; worth remembering if bank-1 status polling ever stalls a set that otherwise
looks healthy.

---

## 12. Signature B, and the DOS call that hid it

Signature B was filed as "PIT hooked but not ticking": `valky_98` hooks INT 08h,
0Bh, 50h and B0h, `opn timer used: false`, exactly one timer IRQ. The diagnostic
also reported `funcvect: -`, which read as "this family has no stub". Both
readings were downstream of something simpler.

`VALKY_98.COM` **is** a stub — 806 bytes that load `CSCP.BIN` from handle 5,
patch it, start it, and then install INT 7Fh and idle:

```asm
01FD  mov dx,0x210
0200  mov ax,0x257f     ; set INT 7Fh -> cs:0x210
0203  int 21h
0205  mov dx,0x7e8
0208  mov al,0x81       ; EXT_STATE = STUB_READY
020A  out dx,al
020B  sti
020C  hlt
020D  jmp 0x20c
```

It never got there. The shell chain reported `Terminated(0)`, and the trace put
the exit at `INT 20h` executed from PSP:0000 — a `.COM` falling off the end of
its own stack. Walking the INT 21h sequence backwards, termination followed
immediately after this:

```asm
01DD  mov bx,0x7        ; handle 7 — nothing in this set binds it
01E0  mov ah,0x3f
01E2  int 21h
01E4  jc  0x1f9         ; error -> skip the play call, install INT 7Fh, idle
```

`AH=3Fh` on a handle nobody opened returned **success with zero bytes**. The
carry stayed clear, so the stub took the "the data is here" branch and handed
its driver a buffer it had never filled; the driver returned into the weeds and
the program died before reaching the line that makes it a stub. Real DOS returns
CF=1 with AX=6, *invalid handle*. Handles 0-4 are the ones DOS always has open,
and an empty read of those is a legitimate EOF — so the error is scoped to
handles 5 and up.

One line, and every `valky_98` set in the queue came back: **8 sets / 177 titles**
(the remaining three are the `(SC-88)` MIDI variants, which were never
candidates — note that the MIDI exclusion lists used for these measurements
match `(SC-55` but not `(SC-88`). 16 of 16 sampled tracks render at −2.3 to
−17.8 dBFS.

The lesson is the one from §10 again, sharper: **`funcvect: -` and "the PIT is
not ticking" were both symptoms of a DOS call answering wrongly two steps
earlier.** Read the shell chain's outcome before reading the capture — a stub
that reports `Terminated` never installed anything, so nothing downstream of it
means what it appears to mean.

### Where the queue stands

Three changes in this pass — the 86-board PCM FIFO, the `.COM` allocation, and
the unopened-handle error — total **32 sets / 772 titles**, with no regression
and no title-count change anywhere in the 337-set sweep.

| stub | sets | titles | signature |
|---|---:|---:|---|
| `emd_98` | 12 | 218 | C — activity finishes before the capture opens |
| `usmd` | 7 | 208 | — |
| `usd_98` | 12 | 208 | — |
| `odq_98` | 5 | 192 | — |
| `magic_98` | 12 | 185 | — |
| `cplay98` | 10 | 156 | — |
| `synup_98` | 6 | 112 | — |
| `ss_98` | 5 | 107 | — |
| `muse_98` | 6 | 106 | — |
| ~30 more | 114 | 2,156 | — |

**194 sets / 3,650 titles**, of which only 5 sets still have a working twin in
the same archive folder — the strongest single hint left, and the one that
caught both of the last two signatures. Signature C (`emd_98`) leads the queue.

### 12.1 — What the handle change costs

The 70-set control is not byte-clean this time, and it should not be: making a
DOS call answer differently changes the cycle count of every program that makes
it. **630 of 688 tracks are byte-identical; 58 differ, across 5 sets.**

The cause is visible in one line of the diagnostic. For Touhou Reiiden the
entire before/after delta is:

```
-    pmd_98   -> StubReady  (1543 cyc)
+    pmd_98   -> StubReady  (1523 cyc)
```

`PMD_98.COM` probes a handle this set does not bind. The probe now returns an
error immediately instead of walking the zero-byte copy path, so the stub is
ready 20 cycles sooner and the capture opens at a slightly different point in
the driver's timer phase.

What that does to the music, measured rather than assumed:

- **39 of the 58** have a **register-write sequence identical to the old rip** —
  only the wait lengths between writes moved.
- The other 19 additionally reorder **6 to 36 writes out of 224,000-384,000**
  (0.003-0.01%), always adjacent updates landing either side of a timer tick.
- Durations are identical to 0.1 s and peak levels agree within 0.3 dB on every
  track sampled.

This is the same effect the §9.4 control saw when the PSP moved three
paragraphs, from a different cause. It is worth restating why it is acceptable
here: the old answer was **wrong**. A read of a handle nobody opened is an
error in DOS, and every program that branches on that carry was being lied to.
Trading a 0.01% phase shift in five sets for eight sets that could not play at
all is the right side of that trade — but the shift is real, and a rip made
before this change will not hash-match one made after.

---

## 13. Signature C — not a capture window, a filename

`emd_98` looked like the capture opening too late: the driver alive with 478
timer IRQs, 839 FM writes, but **1 write captured**, a 10 ms span and no
key-ons. The stub settles it in 144 bytes:

```asm
0147  mov dx,0x180
014A  mov cx,0xffff
014D  mov ah,0x3f
014F  xor bx,bx
0151  int 21h            ; read handle 0 into cs:0x180
0153  jc  0x16d
0155  mov bx,ax          ; bx = bytes read
0157  mov byte [bx+0x180],0   ; NUL-terminate it
015C  mov ah,0x1
015E  int 0xd2           ; EMD "load song file"
0160  cmp al,0
0162  jnz 0x13c          ; failed -> return without playing
0164  mov ah,0x3
0166  int 0xd2           ; play
```

Writing a NUL at the byte count you just read back is only meaningful for a
**string**. `emd_98` is a third `opens_by_name` family: handle 0 carries the
song's *filename*, and the set's gamelist says so — every `.EMI` is a `file` rom
at `offset="-1"` (materialized, never handle-bound) with a matching **`conin`**
rom at the title code. Handed the file's content instead, `INT D2h AH=1` fails,
the stub takes its `jnz` exit, and what is left is the timer the previous call
started, ticking over a driver with no song. That is the "1 write in 10 ms" —
not a window problem at all.

Adding the `emd_98` shell prefix to `opens_by_name` recovers **all 12 sets, 217
of 218 titles**, 18 of 18 sampled tracks at −0.3 to −16.5 dBFS. Nothing else in
the 337-set sweep changed by a single title, which is what a prefix-scoped
change should look like.

That makes **four** families now found to open the song by name — cplay98/FPLAY,
MUSDRV/mbmusp, MDRV acidplan, and EMD. §1 called this "the central discovery of
this campaign"; it has now outlived three separate re-diagnoses, and is worth
checking early whenever a stub reads handle 0 and the driver then reports
failure.

### Where the queue stands after this pass

Four changes today — the 86-board PCM FIFO, the `.COM` allocation, the
unopened-handle error and the `emd_98` filename — recover **44 sets / 989
titles**, with no regression anywhere in the sweep.

| stub | sets | titles |
|---|---:|---:|
| `usmd` | 7 | 208 |
| `usd_98` | 12 | 208 |
| `odq_98` | 5 | 192 |
| `magic_98` | 12 | 185 |
| `cplay98` | 10 | 156 |
| `synup_98` | 6 | 112 |
| `ss_98` | 5 | 107 |
| `muse_98` | 6 | 106 |
| `muspj_98` | 7 | 90 |
| ~35 more | 112 | 1,946 |

**182 sets / 3,432 titles remain**, and the five signatures of §8 are spent:
every one of them turned out to be a DOS or board-level defect rather than the
driver-API gap it was filed as. `cplay98` appearing in the list above is the
next thing worth pulling on — it is a *supported* family failing on 10 sets,
which has so far always meant a per-set binding difference.

---

## 14. Read the guest's console — it was being thrown away

`Pc98RipOutcome::console` came from `String::from_utf8_lossy`. A PC-98 guest
writes **Shift_JIS**, so every Japanese message a driver printed arrived as a
row of replacement characters, and the diagnostic's most direct evidence — the
driver saying in words why it gave up — was unreadable. Decoding it properly
(and stripping the ANSI colour escapes the guests pepper it with) changed the
remaining queue from twenty near-identical "StubReady, no writes" rows into
this:

| stub | titles | what the guest says |
|---|---:|---|
| `usd_98` | 208 | *(harness)* `missing file ILM_03.USO` — a set file absent from the archive folder |
| `odq_98` | 192 | 「サウンドボードがありません！」 — "there is no sound board" |
| `magic_98` | 185 | 「音色が指定されていません」 — "no timbre specified", then resident |
| `cplay98` | 156 | 「常駐に失敗しました。割込み設定をＩＮＴ５に変更してください。」 — "failed to stay resident; change the interrupt setting to INT 5" |
| `synup_98` | 112 | 「内蔵音源ボード(FM6,0188H)」 then `Abnormal program termination` |
| `muse_98` | 106 | `MUSE2 Ver 2.2 installed.` — healthy; its API sits on **INT 05h**, which nothing services |
| `magpa_98` | 77 | 「MPU-PC98 インターフェイスチェック中」 — stalls probing for MIDI |
| `mfd_98` | 69 | `Abnormal program termination` |
| `usmd` | 208 | 「USMD APIが使用可能です」 — resident and healthy; 4 unserviced `INT 7Eh AH=0` |

Two of those name the same missing capability from opposite directions:
`cplay98`'s FPLAY Ver.0 *asks* for the OPN IRQ jumper to select INT 5, and
`muse_98`'s MUSE2 installs its API there. §2's `preset_muse_irq_jumper` already
writes SSG reg 0x0E to steer exactly this choice — it presets `0xC0` (INT 14h)
for MUSDRV. The jumper, not the family, is the unit of work again.

`usd_98` is not an emulation problem at all: a file the gamelist references is
not on disk. Worth checking against the unpack traps in the archive notes
(flattened subdirectories, Shift_JIS names) before assuming the archive is
simply short.

**Decode the console first, next time.** It cost an afternoon of disassembly to
learn things the driver had already printed.

---

## 15. Three things the console named, and the IVT convention behind two of them

### 15.1 — `cplay98`: the board's IRQ jumper

FPLAY Ver.0 printed 「常駐に失敗しました。割込み設定をＩＮＴ５に変更してください。」 —
*failed to stay resident; change the interrupt setting to INT 5* — and exited.
`HOOTRIP_IO_DEBUG=1` shows it reading **SSG reg 0x0E exactly once**: the
PC-9801-26K's IRQ jumper, the same register `preset_muse_irq_jumper` already
writes for MUSDRV. The board's jumper positions are INT0/INT41/INT5/INT6 →
IRQ3/10/12/13 → INT 0Bh/12h/14h/15h, and `0xC0` selects INT5 — which is what
MUSDRV wanted too, from the other end. Extending the preset to the `fplay`
shells makes FPLAY print 「ＩＮＴ５に常駐しました。」 and play.

**10 sets / 139 titles.** The 34 `fplay` sets that already worked were re-swept
with and without the preset: **1,831 audible titles either way**, same status on
every set. The jumper does not disturb a driver that was already happy with
INT0.

Note this did *not* split on the FPLAY build the way `fgplay_h` split on OPNDRV:
six distinct FPLAY.COM binaries are always silent and ten others never are, but
all sixteen are the same 16,138 bytes and half their bytes differ. The jumper
is what separates them, not a version number.

### 15.2 — `magic_98`: one word, two handles

MAGIC_98's INT 7Fh handler reads port 0x7E2 as a **word**:

```asm
in  ax,dx          ; 0x7E2
or  ah,ah
jz  skip_timbre    ; high byte 0 -> no timbre at all
mov bl,ah          ; AH = the DOS handle of the timbre file
... lseek, read, driver call 3 "load timbre" ...
skip_timbre:
mov bl,al          ; AL = the song handle
```

The harness was presenting the low byte alone, so `AH` was zero, the timbre
branch was skipped, and the driver stayed resident printing
「音色が指定されていません」 — *no timbre specified*. Title codes here are
`0x06SS`: byte 1 is the timbre rom's offset, the low byte the song's. Scoped to
the `magic_98` stub deliberately — "a file rom sits at offset byte 1" is true of
**83 sets across seven families**, most of which already play.

### 15.3 — The free-vector convention

Fixing the word was not enough: MAGIC_98 still called `INT EFh`, which nothing
serves, while its driver sat on `INT 6Dh`. Its 282-byte stub says why:

```asm
mov ax,0x35ef      ; get the INT EFh vector
int 21h
cmp bx,0xfff0      ; BIOS dummy?  -> nobody owns EFh
jnz use_ef         ; someone does -> call EFh
mov ax,0x356d      ; else try INT 6Dh
...
```

A PC-98 leaves **unclaimed vectors pointing at the BIOS dummy `IRET` in segment
0xFFF0**, and drivers read that back to find a vector nobody owns.
`install_trampolines` pointed all 256 vectors at our own `TRAMP_SEG`, so every
such probe was told "yes, taken" and the caller then talked to a vector nobody
serves.

Pointing *every* unserviced vector at 0xFFF0 broke 29 sets. MUSIC.COM
(`music_98`) reads INT 48h and treats **segment 0x60** — which `TRAMP_SEG`
happens to be — as its "no resident copy" marker; with 48h moved to the BIOS
dummy it matched INT 0Ah's segment and MUSIC.COM concluded a copy of itself was
already resident, printed so, and exited. Both drivers are right about their own
half of the machine: **PC-98 reserves INT 00h–5Fh for BIOS and DOS, and leaves
60h–FFh as the free application range** — which is exactly where drivers install
their APIs (PMD on 60h, MDRV and EMD on D2h, MAGIC on 6Dh/EFh). So only
unserviced vectors at 0x60 and above read as the BIOS dummy. The dummy handlers
sit at the bottom of segment 0xFFF0 so the top of the ROM — the reset vector and
machine ID at 0xFFFF0 — is left alone.

**12 sets / 175 titles**, and a convention that will matter to every future
driver that asks whether a vector is free.

### 15.4 — Result

**22 sets / 314 titles**, 42 of 42 sampled tracks at −3.2 to −18.5 dBFS, and no
regression or title-count change anywhere in the 337-set sweep.
The 70-set control re-ripped **688 of 688 tracks byte-identical** — unlike the
unopened-handle change, none of these three moves a set that was already
playing. That is what you would expect: two are scoped to one stub each, and the
third only changes what a guest reads back from vectors nothing was serving.

Across the day: **66 sets / 1,303 titles**, leaving **160 sets / 3,091 titles**.

### 15.5 — The queue, and where to start next

157 sets / 3,032 titles (excluding the `(SC-88)` MIDI variants, which the
exclusion lists used earlier in this document did *not* filter — they match
`(SC-55` but not `(SC-88`, and that inflated `valky_98` by three sets).

| stub | sets | titles | what the guest says |
|---|---:|---:|---|
| `usmd` | 7 | 208 | resident and healthy; 4 unserviced `INT 7Eh AH=0` |
| `usd_98` | 12 | 208 | `sound vector 0x15`, one timer IRQ, both OPN timer flags stuck set |
| `odq_98` | 5 | 192 | 「サウンドボードがありません！」 — board detection fails |
| `synup_98` | 6 | 112 | 「内蔵音源ボード(FM6,0188H)」 then `Abnormal program termination` |
| `ss_98` | 5 | 107 | resident, 15 writes, 3 key-ons, 0 s span |
| `muse_98` | 6 | 106 | MUSE2 installs cleanly; its API is on **INT 05h** |
| `muspj_98` | 7 | 90 | 162 timer IRQs, 354 writes, 14 captured |
| `fmxp` | 5 | 83 | `FMX` / `FMXP` run to budget |
| `magpa_98` | 4 | 77 | 「MPU-PC98 インターフェイスチェック中」 — stalls probing MIDI |
| ~30 more | 100 | 1,849 | |

Only **two** silent sets still have a working twin in the same archive folder
(`nlp_hoot`, 63 titles) — that hint, which caught the 86 board and signature D,
is nearly exhausted. The console messages are the live lead now.

`odq_98` is the one to start on: 192 titles behind a driver that says in plain
words it cannot find the sound board, and board detection is a re-host concern
with a history of reaching sets nobody was aiming at.

---

## 16. The queue is 41% MIDI variants, and the FM sibling is not always `.MFM`

### 16.1 — The first full sweep that finished

`pc98-sweep` had never completed. It stalled on `Last Guardian 2: Yomi no
Fuuin`, whose NLP_HOOT stub asks for 64 KB via INT 21h AH=48h; the refusal path
called `max_free_block`, which walked the MCB chain terminating only on a `'Z'`
signature and spun forever once the guest had scribbled the arena. With that
fixed the sweep runs end to end:

**1,941 sets, 1,542 ok, 399 silent, 0 timeout, 0 errored.**

| kind | ok | opna-ext | total |
|---|---:|---:|---:|
| `86` | 76 | 59 | 88 |
| `opn` | 1,266 | 212 | 1,629 |
| `opna` | 200 | 196 | 224 |

**Do not read `0 timeout` as "nothing hung."** The wall-clock deadline is polled
in `pump`, between `cpu.run` calls. The hang above was pure Rust *inside* one of
them — pump's third iteration never began — so no deadline could have reported
it. A spin below that line is invisible to `--deadline` by construction.

### 16.2 — 108 of the 399 are soundtracks we already have

165 of the 399 silent sets carry a `midiout` option, which is the whole of what
`is_fm_variant` tests. Split by whether a working non-MIDI set exists in the
same archive folder:

| | sets | titles |
|---|---:|---:|
| a working non-MIDI twin exists | 108 | ~3,221 |
| no working twin | 57 | ~1,169 |
| not a MIDI variant — the real driver queue | 234 | ~9,154 |

The 108 are not losses. They are the same music counted a second time under its
SC-55/MT-32/GS arrangement, while the `(OPN)` entry beside them already rips.
Counting them as failures inflates every family total that includes them — the
same error §10 caught when `(SC-88)` slipped past an exclusion list and added
three phantom sets to `valky_98`. **The queue is 291 sets, not 399.**

### 16.3 — `.MFM` is one convention out of a dozen

The FM-variant path binds `{stem}.MFM`, the Vermouth/TGLFMP2 convention. Across
the sweep it failed 169 times over 55 archives and 84 distinct song files. For
38 of those 84 a same-stem sibling is sitting in the folder under a different
extension:

| ext | files | | ext | files |
|---|---:|---|---|---:|
| `.FM` | 13 | | `.MD` | 4 |
| `.FMX` | 11 | | `.A` / `.N` / `.26K` / `.M` | 2 each |
| `.GS` | 6 | | `.L` / `.LA` | 1 each |
| `.CM` | 5 | | | |
| `.FM2` | 4 | | | |

The remaining 46 use a different *stem*, not a different extension:
`amrq_98`'s MIDI song is `MR01.TMD` and its FM soundtrack is `MR01F.TMD` — an
`F` suffix. That set also already has a dedicated `(OPN)` entry playing those
`F` files, so the FM-variant path is both unnecessary and wrong for it.

### 16.4 — The `-m` drop fires even when the bind did not

Binding the `.MFM` and dropping `-m` from the fmp shell are a designed pair: the
comment says so. Only the bind is conditional. When the sibling is missing the
code warns, keeps the MIDI song file, and still strips `-m`, so FMP3 installs as
an FM driver holding MIDI data — neither variant's behaviour.

This reaches **4 entries, all `edge98`** (the only warned sets whose shell
starts with `fmp`). `edge98` ships `.M`/`.MD` and has no `.MFM` at all. All five
Edge entries are silent today, including the genuine `(OPN)` one, so the
mis-drive is not currently what keeps them quiet — but it will block recovery
once the underlying cause is fixed.

For the other 169 entries the shell is not `fmp`, so the drop is a no-op and the
only effect is cosmetic: `fm_variant_game_name` strips the `(GS)`/`(MT-32)`
marker and appends `(OPN)`. Harmless while they stay silent, and a false label
the moment one starts producing audio — the failure mode a6a4ce1 fixed for
ADPCM.

### 16.5 — Where to start next, revised

§15.5's ordering counted the 108 duplicates. Revised:

1. **Exclude the MIDI variants that have a working twin.** 108 sets / ~3,221
   titles of phantom queue, and every family total that includes them is wrong
   until this is done. Measurement, not emulation.
2. **The 57 MIDI variants with no working twin** — the only route to that music.
   Fix the pair first (only drop `-m` when the bind succeeded), then teach the
   sibling lookup the `.FM`/`.FMX`/`.M` conventions and the `F`-suffix stem.
   Expect some of the 57 to have no FM soundtrack at all; those should be
   excluded rather than chased.
3. **`odq_98`** — 192 titles, 「サウンドボードがありません！」, unchanged from
   §15.5 and still the best non-MIDI lead.

A caution for whoever takes item 2: *silent* here means title 0 was silent. That
is a weak signal for a set, and on pc88 it has already proved misleading — see
§17.

---

## 17. pc88, which nobody has triaged

Everything above is pc98. The pc88 sweep reads **531 sets, 398 ok, 132 silent,
1 errored**, and no one has taken the silent 132 apart. A first pass says the
pile is smaller than it looks and three of its pieces are not emulation work.

### 17.1 — `silent` here means "title 0 was silent"

`sweep` rips **title 0 only**. That is fine as a smoke test and misleading as
triage, which is worth stating plainly before anyone builds on the number.

The twin heuristic from §11 — a silent set whose same-archive twin rips — looked
like it transferred: 7 silent pc88 sets have a working twin and 6 of the 7 are
the `(OPNA)` side, the same shape as the 86-board split. Ripping them in full
dissolves most of it:

| set | writes | key-ons | verdict |
|---|---:|---:|---|
| `The Scheme (OPNA)` | 48,662 | 1,872 | **more** than its `(OPN)` twin's 9,108 |
| `Kami no Machi (OPNA)` | 7,435 | 420 | more than its twin's 4,192 |
| `Shutendouji (OPNA)` | 0 | 0 | a real zero-write failure |

One genuine case, not seven. Title 0 of those sets is silent; the sets are not.
Any pc88 triage that groups on the sweep's verdict inherits this error — re-rip
before grouping, exactly as §10 says for the stub name.

### 17.2 — 15 sets are not on disk

14 pc88 sets (211 titles) name an archive folder that does not exist in this
snapshot. This is not the unpack trap from the archive notes: extracted folders
and `.zip` files coexist happily here (420 zips under `pc88/`, 1,709 under
`pc98/`), and **none of the 92 absent pc88/pc98dos archives exists as a zip,
lzh or 7z anywhere in the tree**. They are simply not in HootArchive20180626.
Across pc88 and pc98dos that is 92 archives behind ~168 sets — a sourcing
question, not a code one, and it caps what any amount of driver work can reach.

`usd_98`'s missing file in §14 is the same phenomenon one level down.

### 17.3 — One set is lost to a comma

`Emerald Dragon (OPNA)` (71 titles) declares:

```xml
<romlist archive="emdr88,emdr98">
```

`archive` is a **list**. `find_set_dir` lowercases and compares the whole
attribute against each folder name, so a comma can never match and the set is
dropped as folderless. Both folders exist, and the split is real: 47 of the
48 roms are in `emdr88`, and the 48th — `EMVI64.S` — is only in `emdr98`. So
the semantics are "resolve each rom against every listed archive", not "pick
one".

Catalogue-wide this is **35 sets / 817 titles**, and for 33 of them (775 titles)
every named folder is present. Only one is pc88; the other 34 are MSX, whose
pattern is always `<game>_msx,fmpac_msx` — the game plus the FM-PAC BIOS. That
makes the fix worth doing properly rather than special-casing pc88: it is the
mechanism MSX support will need on day one.

### 17.4 — Five driver kinds at 0/1, and the one hard error

| kind | sets | titles | note |
|---|---:|---:|---|
| `8801-11` | 4 | 69 | Romancia, Thexder 88, SeeNa, American Truck |
| `xanadu2` | 1 | 55 | |
| `xanadu` | 1 | 9 | |
| `8801-10` | 1 | 8 | Laptick |
| `asteka2` | 1 | 8 | |

The sweep's `0/1` line for each is itself an artefact of §17.2 — three of the
four `8801-11` sets were skipped for a missing folder, so only one was swept.
Seven of these eight sets are reachable; all seven are silent. These are
bespoke Falcom-era drivers rather than a family, so they are 149 titles behind
five separate problems, not one.

The single hard error is worth more than its size:

```
[PC-8801] Gokudou Jintori (OPN): TITLE.COM does not fit at 0xa000
```

A loader bounds refusal, the same shape as §11.2's `.COM` that would not fit in
65,408 bytes — which turned out to be the loader demanding a round number
rather than the arena being short, and recovered 24 sets once corrected.

### 17.5 — Order

1. **The comma list.** 71 pc88 titles now, 775 catalogue-wide, and MSX cannot
   start without it. Small, well-understood, and the semantics are pinned above.
2. **`Gokudou Jintori`'s loader refusal.** One set, but §11.2 is precedent that
   a loader bound is rarely about the set that reports it.
3. **Re-rip the 132 silent in full before grouping them.** §17.1 says the
   current verdicts cannot carry a triage. Cheap: pc88 sweeps in ~40 s.
4. Leave the absent 14 alone until someone finds a fuller archive.

---

## 18. Title 0 was the measurement, not the music

§17.1 warned that `sweep` rips title 0 only and that no triage should be built
on its verdict. This section measures how wrong that verdict was, fixes the
measurement, and reports what the pc88 queue actually looks like underneath.

### 18.1 — 54 of the 127 silent pc88 sets are not silent

Every set the sweep called silent was re-ripped in full — all titles, 20 s
each, audibility gate unchanged. 137 sets ran (the 126 silent ones plus 11
already-ok sets sharing their archive folders); of the 127 records matching a
sweep `[silent]` line:

| full-rip status | sets |
|---|---:|
| `ok` (every title audible) | 7 |
| `partial` (some titles audible) | 47 |
| `silent` (no title audible) | 73 |

**54 sets, 586 titles, were audible all along.** Title 0 was the only thing
wrong with them. Of the 3,425 titles in those 127 sets, 586 pass the gate, 27
key on without loading a voice, and 317 are ADPCM-only.

Six of the 54 are audible with **zero FM key-ons**: Replicart, Ashe, Super
Mario Bros. Special, Ice Climber, Goonies — 61 titles whose music is entirely
SSG. They are only counted because the gate tests SSG tone as well as FM
key-on, which is the part of `audible.rs` that has been hardest to justify
from FM sets alone.

### 18.2 — The 73 that really are dead

All 73 emit **zero register writes** across every title, which is a much
sharper signature than the mixed pile of 132: nothing reaches the chip at all,
so these are load/trigger failures rather than driver-configuration ones.

| kind | sets |
|---|---:|
| `opn` | 54 |
| `opna` | 19 |

2,073 titles. The largest are `tnmbox_88` Telenet Music Box (OPNA) at 162,
`snatcher` at 123 each side, `destjyo` Destruction Gekan (OPNA) at 80 and
`lizard88` at 77.

**Ten of the 73 have a same-archive twin that rips completely**, so the music
is already in hand and only the board variant fails:

| dead set | titles | working twin |
|---|---:|---|
| Telenet Music Box (OPNA) | 162 | (OPN) 32/162 |
| Destruction Gekan (OPNA) | 80 | (OPN) 80/80 |
| Shin Ku Gyoku Den (OPN) | 51 | (OPNA) 51/51 |
| Yaksa (Music Mode) (OPN) | 21 | Yaksa (OPN) 19/22 |
| RST88 Music Disk #1.3 (SPLIT i2) (OPNA) | 18 | (SPLIT f3) 36/36 |
| Shutendouji (OPNA) | 15 | (OPN) 15/15 |
| Hard Rank (OPNA) | 12 | (OPN) 11/11 |
| Wingman Special (OPNA) | 9 | (OPN) 6/9 |

Note the direction is not fixed: Destruction Gekan and Shutendouji fail on the
OPNA side, Shin Ku Gyoku Den on the OPN side. This is the §11 twin split
again, and unlike the version in §17.1 it now rests on full rips rather than
on title 0, so it can carry a triage.

### 18.3 — The sweep now tries until it hears something

`sweep` and `pc98-sweep` take `--titles` (default 4) and stop at the first
audible title. A healthy set still costs one rip, so only failing sets pay for
the extra attempts:

**pc88: 532 sets, 442 ok, 89 silent, 1 errored** — up from 399 ok / 132 silent,
for 15 s of extra sweep time (29 s → 45 s). The summary line reports how many
of the ok sets needed a later title (43) so the correction stays visible
rather than quietly inflating the number.

Four titles recovers 43 of the 54 the full census found. The remaining 11 hide
their first audible track deeper; the census, not the sweep, is the authority
on any individual set.

### 18.4 — The FM sibling, and the switch that went with it

§16.4's defect is fixed: the `-m` drop is now gated on the `.MFM` bind having
succeeded, so FMP3 can no longer be installed as an FM driver while holding
MIDI data.

§16.3's extension survey is now acted on. The lookup tries `.MFM`, `.MF2`,
`.MF1`, `.FM`, `.FMX` and `.FM2` against the song's stem, which lifts the
midiout sets with a bindable FM arrangement from 32 to 98. Extensions naming a
MIDI *target* — `.GS`, `.CM`, `.LA`, `.MD` — are deliberately excluded, since
binding one recreates exactly the mis-pairing above.

Measured on title 0 only, so the comparison is like for like:

| set group | before | after |
|---|---|---|
| Branmarker 2 (98 + 9821) | 3/11 audible | **11/11** |
| Heart de Ron (98 + 9821 + Yuu Disk 6) | 3/15 | **15/15** |

Not every bind is sufficient: `sp_line_98`, `reno_98` and `lemmona_98` resolve
a sibling and stay silent, so the FM arrangement reaching the driver is
necessary but not the whole story for them.

The `F`-suffix stem convention (`MR01.TMD` → `MR01F.TMD`, §16.3) is still
unimplemented. It is worth less than it looks: `amrq_98`, the set that named
it, already has a dedicated `(OPN)` entry playing those `F` files.

### 18.5 — Order, revised again

1. **The 73 zero-write pc88 sets**, minus the 10 whose twin already covers the
   music — 63 sets, and a single shared symptom to chase rather than 132
   assorted ones.
2. **`Gokudou Jintori`'s loader refusal** (§17.4), unchanged and still the one
   hard error.
3. **Exclude MIDI variants with a working twin from the pc98 queue** (§16.5
   item 1). Now partly overtaken: the sets §18.4 recovered were in that
   bucket, so the count needs redoing after a full pc98 sweep either way.
4. **`odq_98`** — 192 titles, still the best non-MIDI lead.

---

## 19. The pc98 queue, re-measured

The first full pc98 sweep with §18.3's `--titles` and §18.4's sibling list:

**1,941 sets, 1,642 ok, 299 silent, 0 timeout, 0 errored** — up from
1,542 ok / 399 silent. 80 of the newly-ok sets were silent on title 0 and
audible on a later one; the remaining ~20 are FM arrangements that the old
`.MFM`-only lookup could not find.

| kind | ok | opna-ext | total |
|---|---:|---:|---:|
| `86` | 83 | 66 | 88 |
| `opn` | 1,347 | 238 | 1,629 |
| `opna` | 212 | 208 | 224 |

### 19.1 — Re-bucketed

§16.2's split, recomputed on the 299:

| | sets | titles |
|---|---:|---:|
| MIDI variant, a working non-MIDI twin exists | 110 | 2,886 |
| MIDI variant, no working twin | 36 | 640 |
| **not a MIDI variant — the real driver queue** | **154** | **5,718** |

The real queue was 234 sets / 9,154 titles in §16.2. It is now 154 / 5,718,
and the duplicate bucket grew because recovering a set makes its MIDI siblings
duplicates rather than losses.

### 19.2 — Four sets are half the queue

| titles | archive | set |
|---:|---|---|
| 898 | `metajo2_98` | Zwei Metajo (OPNA+SSGPCM) |
| 898 | `metajo2_98` | Zwei Metajo (86) |
| 514 | `metajo_98` | Metajo (OPNA+SSGPCM) |
| 514 | `metajo_98` | Metajo (86) |

**2,824 of the 5,718 titles — 49% — sit in two archives.** Every other set in
the queue is under 65 titles. `odq_98`, §16.5's best lead at 192 titles, is no
longer close to the top; these two archives are worth fifteen of it, and they
are two problems rather than four, since each is one archive ripped under two
board variants.

Whether that is 2,824 titles of distinct music is the first thing to check —
a set declaring 898 titles is unusual enough to warrant confirming the title
list is not enumerating something other than songs.

### 19.3 — One gamelist is not Shift_JIS

`xml2/zzz_vermouth.xml` is UTF-8 with a BOM, while the other 556 gamelists are
Shift_JIS. `decode_bytes` already handles it correctly — it reads the declared
`encoding=` and only assumes Shift_JIS when none is given — but any *analysis*
script that hardcodes Shift_JIS drops this one file silently, and with it all
48 Vermouth GUS-simulation entries. That is exactly what happened while
bucketing the queue above, and the missing 48 showed up only as a count that
did not add up.

### 19.4 — Order

1. **`metajo_98` / `metajo2_98`** — 2,824 titles across two archives, after
   confirming the title lists mean what they say.
2. **The 73 zero-write pc88 sets**, minus the 10 their twins already cover
   (§18.2).
3. **`odq_98`** — 192 titles, 「サウンドボードがありません！」.
4. **The 36 MIDI variants with no working twin**, of which some will have no
   FM arrangement in this archive at all and should be excluded rather than
   chased.

### 19.5 — What `metajo` looks like when it fails

A first diagnostic on `Zwei Metajo (86)`, recorded so whoever takes item 1
does not start from nothing. The titles are real: `metajo2_98` holds 880 files,
776 `.PCH` and 100 `.PKO`, which is where 898 comes from — this is a music
collection, not a mis-parsed title list.

The re-host itself succeeds. `PCP /S` and `PCML /S /M16` both go resident, the
`PKO_98` stub reports ready, INT 7Fh is installed and the trigger runs. Then:

```
FM writes    : 44 total, 34 captured
write span   : 0.000s .. 0.001s  (of 5.0s capture)
reg 0x27 timer-ctrl writes: 1   values: 0x30×1
timer IRQs   : 0        opn timer used: false
unmodelled ports: 0x0088(r0/w2) 0x008a(r3/w2) 0xa66e(r1/w0)
```

The driver touches the chip once and stops. Reg 0x27 is written only with
`0x30` — clear both timer flags — so timer A/B are never programmed and
nothing paces a sequence. That is §8's bucket B shape (pacing source hooked
but not running), except no pacing source is hooked at all.

The unmodelled ports are the more interesting half. `0xa66e` is in the
PC-9801-86 PCM range, and the driver chain is `PCP`/`PCML`/`PKO` with the
sibling entry named `(OPNA+SSGPCM)`. This set's music may be substantially
PCM — 86-board PCM on one variant, SSG-volume PCM on the other — in which
case the FM-write count is the wrong thing to be watching and both an
unmodelled port and an audibility question are in play before any driver
work starts.
