---
title: Silent rips — measurement and causes
---

# Silent rips — how many, and why

A full archive rip produces well-formed S98/VGM files that a player will happily
open, seek in, and show a duration for — and that emit nothing. This note records
how the problem was measured, the classifier that now catches it, and what the
remaining causes are.

All figures come from one complete `archive-rip` of the 2018 HootArchive:
**2,213 set folders, 62,425 tracks**.

---

## 1. Why it went unnoticed

The rip loop called a title silent only when it produced **zero** register
writes. A sound driver that initialises the chip — timers, mode bits, LFO, a
bank of maximum-attenuation TLs — and then never sequences a note clears that
bar comfortably. Its capture was written to disk and its set recorded as `ok`.

The manifest therefore listed 58,821 tracks under `ok` and **zero** under
`silent`, while a quarter of the corpus made no sound.

## 2. The audibility gate

`hoot-log`'s [`audible`](https://github.com/TheWhyteWolf/hootrip/blob/main/crates/hoot-log/src/audible.rs)
module classifies a log on activity that can actually reach the output:

| Class | Meaning |
|---|---|
| `Dead` | nothing keyed on, no SSG tone, no rhythm |
| `NoVoice` | key-ons, but every operator TL left at maximum attenuation |
| `AdpcmOnly` | only ADPCM-B control (see §4) |
| `Audible` | something that can make a sound |

Two rules follow chip *state* rather than raw writes, because both shapes occur
in the wild and both render to pure zero:

- **SSG mixer.** A level write to reg 0x08–0x0A only counts if the mixer
  (reg 0x07, **active-low**) lets that channel's tone or noise through. One set
  writes mixer `0xBF` — every tone and noise channel muted — and then 66
  non-zero level writes. It is silent.
- **SSG envelope mode.** Bit 4 of a level register hands amplitude to the
  envelope generator and makes the level bits meaningless. A channel parked at
  `0x10` with no envelope shape (reg 0x0D) ever written produces nothing.

Both also re-evaluate retroactively: unmuting a channel, or starting the
envelope, can give voice to a level that was latched earlier.

### Where the gate is applied

Every path that writes a track classifies it first: `archive-rip` (both the pc88
and pc98dos loops), `rip`, `pc98-rip`, and the two sweeps. `pc98-rip` was the
exception until 2026-09-16 — it still used the original "did it write anything"
test, so a single-set rip shipped files the batch rip would have dropped. The
tell is a track with a few dozen writes and no notes: the NL family's
`音色定義` timbre-definition pseudo-tracks program ~60 registers, key on
nothing, and rendered to digital zero.

If you are validating a fix by ripping one set, use a build after that date, or
run `hootrip triage` over the output — otherwise a designed-silent pseudo-track
reads as a failure of the fix.

### Validation

Verdicts were checked against audio rendered by libvgm (`vgm2wav --loops 1
--fade 0`) on fresh random samples:

| Class | Rendered inaudible |
|---|---|
| `Dead` | 35 / 35 |
| `NoVoice` | 20 / 20 |
| `AdpcmOnly` | 19 / 20 (see §4) |
| `Audible` | 0 / 60 |

## 3. The measured split

```
audible           46158  (73.9%)
dead (no key-on)  13123  (21.0%)
no voice loaded    1505  ( 2.4%)
ADPCM-only         1639  ( 2.6%)
SILENT total      16267  (26.1%)
```

Reproduce it on any output tree, with no archive present:

```sh
hootrip triage out --report triage.jsonl
```

## 4. ADPCM is a format gap, not a failed rip

OPNA ADPCM-B samples reach the chip by DMA from host RAM. A register log does
not see that traffic, so the log holds the control writes and none of the audio.
Across all **1,639** ADPCM-only tracks, **not one** carried its samples inline —
zero writes to the port-1 data register 0x08. None is reproducible from the log
alone, in either output format.

There is a trap for anyone spot-checking by ear: **635** of those tracks issue a
START (reg 0x00 bit 7) over ADPCM RAM that was never filled, and a renderer will
play that uninitialised memory as loud noise. One sampled track peaks at
−2.1 dBFS of pure garbage. Loud output in this class is not recovered music,
which is why it counts as silent.

## 5. Song selection that never takes effect

Digesting each file's **dump region** (the command stream, excluding the tag
block that carries the per-track title) shows how many tracks in a set are
byte-identical:

- **248 sets have every track identical** — 121 pc88, 127 pc98 — covering 5,033
  tracks.
- A further **584 sets** contain smaller duplicate groups.

Grouped by what those identical tracks contain, the 248 split cleanly in two,
and the split names two different bugs:

| Dominant class | Sets | Reading |
|---|---|---|
| `dead` | 168 | no song data ever reached RAM; the driver inits and idles |
| `audible` | 53 | a song loads and plays, but always the *same* one |
| `adpcm_only` | 25 | ADPCM set, per §4 |
| `novoice` | 2 | — |

**The 168 dead sets** were the documented gap in
`crates/hoot-machine/src/pc88.rs`, and are now largely fixed. The bgm bank was
only copied into RAM when the set declared `mdata_addr`; where it did not, the
song was never placed anywhere, the driver played nothing, and since the title
code then changed no machine state, every title captured an identical dead log.

hoot's PATCH hands the driver a pointer to the song data before calling it —
`agni`'s 56-byte stub does exactly this:

```
F014: DB 00        IN A,(0x00)      ; poll for the play command
F019: FE 01        CP 1
F01D: DB 01        IN A,(0x01)      ; song number
F021: DD 21 00 40  LD IX,0x4000     ; where the song data must live
F025: CD D6 12     CALL 0x12D6      ; driver play entry
```

Reading that address back out statically does **not** work. A linear Z80 walk
over all 546 pc88 sets, taking the last 16-bit register load before the driver
call, agreed with the declared `mdata_addr` on 14 sets and disagreed on 37.

So the address is not deduced, it is **tried**. When a set declares no data
address and the plain capture comes out silent, candidate addresses are probed
with a short run, and one is accepted only if the capture actually makes a sound
— the audibility classifier from §2 is the acceptance test. Sets that already
capture sound never reach the probe, so they are bit-for-bit unaffected.

`0x4000` is the dominant convention and currently the only candidate; sweeping
`0x1000`–`0xC000` over the sets it does not fix recovers nothing, so a longer
list would be speculation. `hootrip rip --mdata-addr 0x4000` forces it by hand,
which is how the convention was found.

**Measured against the restored archive:**

| | Result |
|---|---|
| Target sets (previously 100% dead) | **40 of 65 now produce audible music** |
| Tracks recovered | **719**, from a baseline of zero |
| Sets still silent | 25 — now reported `silent`, no longer `ok` |
| Control sets (previously ripping fully) | 183 of 188 **unchanged** |

Every one of the five control differences was explained and none was caused by
the change: four differ only in title count because the re-downloaded archive is
a slightly different revision (Misty Blue *gains* tracks, 4 → 29), and one rips
one fewer track because the audibility gate correctly drops a dead title.

**The 53 audible sets** are a different problem: per-game drivers. hoot does not
drive these through one generic protocol. Its 2001 source release registers 24
bespoke drivers as `majortype/subtype` pairs, each with its own trigger:

```
mucom88/{generic,ys,ys2,ys3,scheme,ed2,sorcva,mistyblue,yk-2opn,yk-2disk,x1,x1psg,pc98}
wolfteam/midgarts   enix/angelus    xtalsoft/{xtaljbox,battlegorilla}
gamearts/{silpheed,firehawk}        glodia/zavas    arsys/starcruiser
konami/snatcher     telenet/tnmbox  scaptrust/starship  trpscr/{generic,revolter}
x68k/{generic,mxdrv}   msx/{kss,scc,ds4,konami_psg}    pcengine/hes
```

Named all-identical sets match that list directly — Mid-Garts (4 sets, 40 tracks
each), Angelus (2 × 36), The Scheme (80), Fire Hawk (2 × 14).

Crucially, hoot's own `Mucom88Driver::Play()` does **not** use a virtual-port
handshake. It writes driver-specific RAM addresses and, for several subtypes,
`memcpy`s the song bank in and resets the Z80:

```cpp
case 0x00:                      // TYPE_GENERIC
    ram[PLAY_FLAG] = 0x01;      // 0xC010
    ram[PLAY_CODE] = _code;     // 0xC011
```

hootrip's PC-88 harness implements an inferred PATCH/virtual-port protocol
instead. Reconciling it with the real per-driver trigger table is the next piece
of work, and it needs the archive present to verify.

> The 2001 source predates the 2018 archive by seventeen years and covers only
> 24 drivers; name-matching accounts for 13 of the 248 sets. It establishes the
> *shape* of the protocol, not the full table.

## 6. Some duplicate tracks are the catalogue's fault

Before hunting a bug in song selection, check the gamelist. **177 of the 7,641
multi-title games (2.3%) declare the same `<title code=…>` twice or more** — 454
redundant titles in total. `heartsoft.xml` gives SAD.X, TEESE.X and TINA.X all
`code="0x09"`, so Paragon Sexa Doll can only ever produce ten distinct songs from
twelve titles. That is correct behaviour on bad input, not a selection failure.

It is a small slice of the duplicate problem overall (7 pc88 and 16 pc98dos
games; the worst offenders are arcade drivers), but it is worth ruling out first
because it looks exactly like a rip bug.

Note also that `archive-rip --only-archives` selects by **romlist archive
folder**, and several catalogue entries can share one folder — asking for
`100yen2` also rips the Onion Software entry alongside the Jumper 2 one. A/B
comparisons built from archive names will pull in sets you did not choose.

## 7. What is not wrong

- **S98 and VGM disagree by ~6 dB on purpose.** `--headroom-db` (default 6.0)
  is applied through the VGM volume-modifier byte at header `0x7C`. S98 has no
  equivalent field, so `.s98` files run correspondingly hotter and many peak
  near 0 dBFS. This is an asymmetry in the formats, not a defect in either
  writer.
- **Both writers were independently verified** against libvgm; the register
  streams they emit round-trip and render identically apart from that gain.
