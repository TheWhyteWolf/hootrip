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

## 4. The remaining silent sets — a ranked queue

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

