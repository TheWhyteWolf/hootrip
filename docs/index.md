---
title: hootrip
description: Batch converter from hoot emulator music sets to S98 / VGM register logs
---

# hootrip

**hootrip** converts [hoot](http://dmpsoft.s17.xrea.com/hoot/) emulator music
sets into **S98 v3** and **VGM 1.71 / .vgz** register-log files — headless, in
batch, at full timing resolution.

hoot plays Japanese computer game music (PC-8801, PC-9801, X68000, MSX, …) by
emulating the original hardware and running each game's own sound driver. Its
built-in logger only captures at 100 Hz, drops ADPCM, and has to be driven by
hand through the GUI. hootrip rebuilds that pipeline as an open tool: it reads a
set from the hoot XML catalogue, re-hosts the sound driver under its own
emulation, and logs every chip register write.

> hootrip produces only register logs. It contains no game ROM, sound data, or
> copyrighted music — you supply those from your own hoot archive.

## What works today

- **Catalogue** — parses the full HootArchive (`hoot.xml` + 557 Shift_JIS
  gamelists, 8096 entries) with zero errors, including title-code expansion and
  set-folder resolution.
- **Writers** — S98 v3 and VGM 1.71 output with loop points, S98/GD3 tags, `.vgz`
  gzip, and automatic loop detection; independently verified against libvgm.
- **PC-88** — a Z80 + OPN/OPNA re-host of hoot's PATCH bootstrap; ~82% of sets
  rip cleanly.
- **PC-98 DOS** — a full MS-DOS re-host on the NP2 i286 core covering OPN and
  OPNA (PC-9801-86) sets across many driver families (PMD, cplay98, artdi,
  mbmusp, mdrv, MDR external-voice, MUSE/NMUSE, …).
- **Archive rip** — one command batch-rips the whole catalogue with process
  isolation per set, a parallel worker pool, and a resumable census manifest.

## Documentation

- [PC-98 driver families](pc98-driver-families.html) — how each sound-driver
  family delivers a song and how the harness reproduces it.
- [Silent rips](silent-rips.html) — measuring which captures actually make a
  sound, the audibility classifier, and the causes of the ones that do not.
- [Development notes](DEVELOPMENT.html) — building, verification tooling, and the
  ground-truth methodology.

## Source

The project lives on [GitHub](https://github.com/TheWhyteWolf/hootrip). It is a
Rust workspace (`hoot-xml`, `hoot-log`, `hoot-chips`, `hoot-cpu`, `hoot-machine`,
`hootrip-cli`); build with `cargo build --release` and run the `hootrip` binary
against your archive.
