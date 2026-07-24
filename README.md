# hootrip

Batch converter from [hoot](http://dmpsoft.s17.xrea.com/hoot/) emulator music
sets to **S98 v3** and **VGM 1.71 / .vgz** register-log files.

hoot plays Japanese computer game music (PC-8801, PC-9801, X68000, MSX, …) by
emulating the original machine and running the game's own sound driver. Its
built-in S98 logger is limited to 100 Hz timing, no ADPCM capture, and manual
GUI operation. hootrip rebuilds that pipeline as an open, headless, batch tool:
load a set as described by the hoot XML catalogue, re-host its sound driver under
our own emulation, and log the chip register writes at full resolution.

Only register logs are produced (S98 / VGM). No game ROM, sound data, or
copyrighted musical work is included or distributed — you supply those from your
own legally obtained hoot archive.

## Status

- **`hoot-xml`** — parses the full HootArchive catalogue (`hoot.xml` + 557
  Shift_JIS gamelists, 8096 game entries, 0 errors): bind rules, title-code
  expansion, set-folder resolution.
- **`hoot-log`** — S98 v3 and VGM 1.71 writers with loop points, S98/GD3 tags,
  `.vgz` gzip, loop detection, and an S98 reader + register-stream comparator.
  Validated against libvgm (`vgm2wav` renders both formats with matching audio).
- **`hoot-machine` / PC-88** — Z80 + an OPN/OPNA bus front-end re-hosting hoot's
  PATCH bootstrap protocol (virtual trigger ports, IM 2 VRTC/clock/INT4 pacing,
  `use_rtc`/`use_vrtc`, IRQ ack/mask). CPU and OPN run as separate clock domains;
  timing validated against hoot's own source. ~82% of PC-88 sets rip cleanly.
- **`hoot-machine` / PC-98** — a genuine MS-DOS re-host on the vendored NP2 i286
  core: virtual DOS working directory, device-driver INIT, shell-command chain,
  and hoot's `externalCommand` (INT 7Eh/7Fh) song-trigger protocol. Handles OPN
  and OPNA (PC-9801-86) sets and a broad range of driver families (PMD, cplay98,
  artdi, mbmusp, mdrv, MDR external-voice, MUSE/NMUSE, and more — see
  [docs/pc98-driver-families.md](docs/pc98-driver-families.md)).
- **Archive rip** — `hootrip archive-rip` batch-rips the whole catalogue: one
  isolated process per set, a bounded parallel worker pool, a resumable JSONL
  census manifest, and stop-track–aware track selection.

## Usage

```sh
# Point --archive at your unpacked HootArchive (the folder with hoot.xml).
hootrip --archive /path/to/HootArchive stats                 # catalogue coverage
hootrip --archive /path/to/HootArchive list --platform pc98dos
hootrip --archive /path/to/HootArchive show "Markadia"

# Rip one set (all songs) to out/<platform>/<game>/NN title.{s98,vgz}
hootrip --archive /path/to/HootArchive pc98-rip "Markadia (OPNA)" --out out
hootrip --archive /path/to/HootArchive rip "The 4th Unit (OPN)" --out out   # PC-88

# Batch-rip the whole archive with a resumable census manifest
hootrip --archive /path/to/HootArchive archive-rip --out out --jobs 8
hootrip --archive /path/to/HootArchive archive-rip --dry-run                # plan only

# Validate a rip against a reference register log
hootrip --archive /path/to/HootArchive compare "The 4th Unit (OPN)" \
        --reference ground_truth.s98
```

Each `archive-rip` set records one JSON line in `out/manifest.jsonl`
(writes, key-ons, loop found, stop-tracks skipped, status), giving a full census
of what ripped and what did not.

## Layout

- `crates/hoot-xml` — hoot gamelist XML / archive catalogue parsing
- `crates/hoot-log` — register-log model, S98/VGM/VGZ writers, S98 reader, compare
- `crates/hoot-chips` — chip bus front-ends (timers / status / IRQ; no synthesis)
- `crates/hoot-cpu` — CPU cores behind a common trait (Z80, NP2 i286)
- `crates/hoot-machine` — per-platform harnesses (PC-88, PC-98 DOS)
- `crates/hootrip-cli` — the `hootrip` binary
- `vendor/iz80` — patched Z80 core (BSD-3)
- `vendor/np2` — NP2 i286 CPU core

See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for build notes and verification
tooling, and [docs/pc98-driver-families.md](docs/pc98-driver-families.md) for the
per-driver re-hosting details.

## Format references

- S98 v3: <https://vgmrips.net/mirror/s98spec3.txt>
- VGM 1.71: <https://vgmrips.net/wiki/VGM_Specification>
- hoot XML: no formal spec; the DTD embedded in every gamelist file is the
  de-facto schema.

## License

MIT — see [LICENSE](LICENSE). Vendored CPU cores retain their own licenses.
