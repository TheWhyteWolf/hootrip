---
title: Development notes
---

# Development notes

Practical notes for building on and hacking hootrip.

## Gotchas

- **The hoot gamelist XML is Shift_JIS with CRLF.** All 557 files under
  `xml/` and `xml2/` are Shift_JIS, not UTF-8 — `hoot-xml`'s `decode_bytes()`
  handles this; never read them as UTF-8 directly. Note that some `grep`
  builds (notably `ugrep`) silently skip or mismatch Shift_JIS files and exit
  non-zero even when an ASCII pattern is present. Use GNU `grep`, `grep -a`, or
  Python for archive text searches.
- Point the tool at your own unpacked HootArchive with `--archive <path>`; that
  directory holds `hoot.xml` plus the `xml/`, `xml2/`, and per-platform data
  folders. **The presence of a folder named `HootArchive…` proves nothing** —
  check for `hoot.xml` itself. Without the catalogue the tool cannot enumerate
  or bind anything, and `hootrip triage` is the only subcommand that still runs.
- **hoot's own sources are Shift_JIS too**, so the `ugrep` trap above applies to
  them as well as to the gamelists — decode explicitly when searching them.

## Verification tools

- `vgm2wav` / `vgmplay` from system libvgm render both `.vgm`/`.vgz` and `.s98`,
  so they double as an independent check on the writers
  (`cargo run -p hoot-log --example gen_test`).
- `hootrip sweep` — PC-88 pass/fail survey; `hootrip pc98-sweep` — the PC-98
  equivalent, bucketed by driver kind.
- `hootrip compare <game> --reference ref.s98` — LCS-aligns two register
  streams and reports tempo ratio and timing error; ready for any reference
  `.s98`.
- **Ground truth is hoot's own source, not a running hoot.** hoot is
  closed-source and its S98 logger / IPC don't automate headlessly. The source
  release (`dmpsoft.s17.xrea.com/data/hootsrc20011006.cab`, extract with
  `cabextract`) defines the exact machine model each driver runs on — the useful
  files are `drivers/mucom88.cpp` (PC-88 machine setup) and
  `sound/ssFMTimer.cpp` (OPN timer periods). PC-98 rips can also be cross-checked
  against the NP2-sourced 1000 Hz rips on s98.joshw.info.

## Style

- Register-log writers stay pure: bytes in → bytes out, no I/O, so they are
  unit-testable against the format specs.
- Vendored C/C++ cores (the NP2 i286 CPU under `vendor/np2`) are compiled via a
  `cc` + `glob` `build.rs` and wrapped with hand-written FFI.
