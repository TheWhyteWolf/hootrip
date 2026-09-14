use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hoot_xml::Catalogue;

#[derive(Parser)]
#[command(name = "hootrip", about = "Convert hoot emulator sets to S98/VGM", version)]
struct Cli {
    /// Path to the unpacked hoot archive root (contains hoot.xml)
    #[arg(long, global = true, default_value = ".")]
    archive: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Parse the whole catalogue and report coverage statistics
    Stats,
    /// List games matching a platform and/or name substring
    List {
        /// Filter by driver platform (e.g. pc88, pc98dos, x68k)
        #[arg(long)]
        platform: Option<String>,
        /// Case-insensitive name substring
        #[arg(long)]
        name: Option<String>,
    },
    /// Show one game entry in detail (roms, options, expanded titles)
    Show {
        /// Exact or substring match on game name
        name: String,
    },
    /// Smoke-sweep every pc88 set: rip title 0 briefly, tally pass/fail
    Sweep {
        /// Seconds to rip per set (short — this is a diagnostic, not a real rip)
        #[arg(long, default_value_t = 4.0)]
        seconds: f64,
        /// Only sweep sets whose name contains this substring
        #[arg(long)]
        filter: Option<String>,
        /// Print each failing set's diagnostics
        #[arg(long)]
        verbose: bool,
    },
    /// Compare our rip of a title against a reference S98 log (ground truth)
    Compare {
        /// Substring match on game name (must be a pc88 set)
        game: String,
        /// Title index to rip (see `show`)
        #[arg(long, default_value_t = 0)]
        index: usize,
        /// Reference .s98 file to compare against
        #[arg(long)]
        reference: PathBuf,
        /// Emulated seconds to rip (default: match reference length)
        #[arg(long)]
        seconds: Option<f64>,
    },
    /// Diagnose a pc98dos set: run its shell chain and report what the driver
    /// did (hooked vectors, DOS calls, ports, capture activity)
    Pc98 {
        /// Substring match on game name (must be a pc98dos set)
        game: String,
        /// Title index to select (see `show`)
        #[arg(long, default_value_t = 0)]
        index: usize,
        /// Emulated seconds to run the capture phase
        #[arg(long, default_value_t = 5.0)]
        seconds: f64,
        /// Emulated seconds each shell command may run
        #[arg(long, default_value_t = 3.0)]
        setup_seconds: f64,
        /// Force the timer/sound IRQ vector instead of auto-detecting it
        #[arg(long)]
        sound_vector: Option<String>,
        /// Single-step-trace this shell command (0-based) instead of ripping,
        /// reporting hot PCs / INT sequence to diagnose a stall
        #[arg(long)]
        trace: Option<usize>,
        /// Max instructions to single-step when --trace is set
        #[arg(long, default_value_t = 5_000_000)]
        trace_steps: u64,
        /// Override the traced command line (diagnostic; e.g. "pmd /k")
        #[arg(long)]
        cmd: Option<String>,
        /// Trace the funcvect capture phase (driver API calls, reads, hot PCs)
        #[arg(long)]
        trace_capture: bool,
    },
    /// Smoke-sweep pc98dos sets: rip title 0 briefly, tally pass/fail by driver
    /// kind, to see which families produce audio
    Pc98Sweep {
        /// Seconds to rip per set (short — diagnostic, not a real rip)
        #[arg(long, default_value_t = 10.0)]
        seconds: f64,
        /// Only sweep sets whose name contains this substring
        #[arg(long)]
        filter: Option<String>,
        /// Only these comma-separated driver kinds (default: opn,opna,86)
        #[arg(long, default_value = "opn,opna,86")]
        kinds: String,
        /// Cap the number of sets swept (0 = all)
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Skip the first N matching sets (for process-isolated driving)
        #[arg(long, default_value_t = 0)]
        skip: usize,
        /// Emit one tab-separated `status\tkind\text\tname` line per set and no
        /// summary (for aggregating a process-per-set sweep)
        #[arg(long)]
        oneline: bool,
        /// List every failing set
        #[arg(long)]
        verbose: bool,
    },
    /// Rip a pc98dos game's songs to S98/VGZ (per-title files, GD3/S98 tags,
    /// loop detection + fade metadata, output headroom)
    Pc98Rip {
        /// Substring match on game name (must be a pc98dos set)
        game: String,
        /// Rip only the title at this index (see `show`); default all titles
        #[arg(long)]
        index: Option<usize>,
        /// Emulated seconds to record per song (long enough to catch the loop)
        #[arg(long, default_value_t = 210.0)]
        seconds: f64,
        /// Output directory (files land in <out>/pc98/<game>/)
        #[arg(long, default_value = "out")]
        out: PathBuf,
        /// Output formats: s98, vgz, or both
        #[arg(long, default_value = "both")]
        format: String,
        /// Output headroom in dB, applied via the VGM volume modifier (0 = off)
        #[arg(long, default_value_t = 6.0)]
        headroom_db: f32,
        /// Minimum loop length (seconds) for loop detection
        #[arg(long, default_value_t = 5.0)]
        min_loop: f64,
        /// Disable loop detection (keep the full fixed-length capture)
        #[arg(long)]
        no_loop: bool,
        /// Override the OPN(A) chip clock in Hz (paces tempo + declared clock);
        /// default is per-kind (OPN 3993600 / OPNA 7987200)
        #[arg(long)]
        opn_clock_hz: Option<u32>,
        /// Print per-title diagnostics
        #[arg(long)]
        verbose: bool,
    },
    /// Rip a game's songs to S98/VGZ (pc88 only for now)
    Rip {
        /// Substring match on game name
        game: String,
        /// Rip only the title at this index (see `show`); default all titles
        #[arg(long)]
        index: Option<usize>,
        /// Emulated seconds to record per song
        #[arg(long, default_value_t = 120.0)]
        seconds: f64,
        /// Output directory
        #[arg(long, default_value = "out")]
        out: PathBuf,
        /// Output formats: s98, vgz, or both
        #[arg(long, default_value = "both")]
        format: String,
        /// Print IRQ/port diagnostics per title
        #[arg(long)]
        verbose: bool,
    },
    /// Rip one catalogue entry addressed by its ordinal (index into the loaded
    /// catalogue). This is the isolated per-set unit `archive-rip` forks; it
    /// writes song files under <out> and prints a one-line JSON summary. Rarely
    /// run by hand — use `archive-rip` (or `pc98-rip`/`rip` for a named set).
    RipOne {
        /// Catalogue ordinal to rip (as enumerated by `archive-rip`)
        #[arg(long)]
        ordinal: usize,
        /// Output directory (files land in <out>/<platform>/<game>/)
        #[arg(long, default_value = "out")]
        out: PathBuf,
        /// Emulated seconds to record per song
        #[arg(long, default_value_t = 210.0)]
        seconds: f64,
        /// Output formats: s98, vgz, or both
        #[arg(long, default_value = "both")]
        format: String,
        /// Output headroom in dB (VGM volume modifier; 0 = off)
        #[arg(long, default_value_t = 6.0)]
        headroom_db: f32,
        /// Minimum loop length (seconds) for loop detection
        #[arg(long, default_value_t = 5.0)]
        min_loop: f64,
        /// Disable loop detection (keep the full fixed-length capture)
        #[arg(long)]
        no_loop: bool,
        /// Self-terminate a spinning capture after this many wall-clock seconds
        #[arg(long)]
        deadline: Option<f64>,
    },
    /// Batch-rip the whole archive: one isolated child process per set (so a
    /// hang or crash can't poison the shared CPU singleton or the run), a
    /// bounded worker pool, and a resumable JSONL census manifest.
    ArchiveRip {
        /// Output directory (files land in <out>/<platform>/<game>/)
        #[arg(long, default_value = "out")]
        out: PathBuf,
        /// Manifest path (default <out>/manifest.jsonl); one JSON record per set
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Comma-separated driver platforms to include
        #[arg(long, default_value = "pc98dos,pc88")]
        platforms: String,
        /// Comma-separated chip driver kinds to include
        #[arg(long, default_value = "opn,opna,86")]
        kinds: String,
        /// Parallel worker processes (0 = auto: min(8, CPUs))
        #[arg(long, default_value_t = 0)]
        jobs: usize,
        /// Emulated seconds to record per song
        #[arg(long, default_value_t = 210.0)]
        seconds: f64,
        /// Absolute per-set wall-clock ceiling in seconds. The effective budget
        /// is `songs * title_deadline` capped by this, so a large healthy set
        /// gets the time it needs while one pathological set can't hog a worker
        /// indefinitely.
        #[arg(long, default_value_t = 1800.0)]
        timeout: f64,
        /// Per-song wall-clock spin-guard in seconds. A single song that runs
        /// this long (a driver stuck in a busy loop) is abandoned; healthy songs
        /// finish in well under a second to a few seconds, so keep margin.
        #[arg(long, default_value_t = 45.0)]
        title_deadline: f64,
        /// Output formats: s98, vgz, or both
        #[arg(long, default_value = "both")]
        format: String,
        /// Cap the number of sets processed this run (0 = all outstanding)
        #[arg(long, default_value_t = 0)]
        limit: usize,
        /// Restrict to sets whose romlist archive folder name appears in this
        /// file (one name per line, '#' comments allowed). Case-insensitive.
        #[arg(long)]
        only_archives: Option<PathBuf>,
        /// Ignore the existing manifest and re-rip every matching set
        #[arg(long)]
        no_resume: bool,
        /// Also re-run sets previously recorded as error/timeout
        #[arg(long)]
        retry_failed: bool,
        /// Print the plan (target count, breakdown) and exit without ripping
        #[arg(long)]
        dry_run: bool,
    },
    /// Classify already-exported rips by whether they can actually make sound,
    /// and flag sets whose songs are all byte-identical. Reads only the output
    /// tree — no hoot archive required.
    Triage {
        /// Directory of exported rips (an `archive-rip --out` tree)
        dir: PathBuf,
        /// Write a per-track JSONL report here
        #[arg(long)]
        report: Option<PathBuf>,
        /// Directory to move silent tracks into (with their .vgz twins)
        #[arg(long)]
        quarantine: Option<PathBuf>,
        /// Actually move the files. Without this the command only reports.
        #[arg(long)]
        apply: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // Triage inspects exported rips only. It must run with no hoot archive
    // present, so it is dispatched before the catalogue is loaded.
    if let Cmd::Triage { dir, report, quarantine, apply } = &cli.cmd {
        return triage(dir, report.as_deref(), quarantine.as_deref(), *apply);
    }
    let archive_path = cli.archive.clone();
    let cat = Catalogue::load(&archive_path)?;

    match cli.cmd {
        Cmd::Stats => stats(&cat),
        Cmd::List { platform, name } => list(&cat, platform.as_deref(), name.as_deref()),
        Cmd::Show { name } => show(&cat, &name),
        Cmd::Rip { game, index, seconds, out, format, verbose } => {
            rip(&cat, &game, index, seconds, &out, &format, verbose)?
        }
        Cmd::Compare { game, index, reference, seconds } => {
            compare_cmd(&cat, &game, index, &reference, seconds)?
        }
        Cmd::Sweep { seconds, filter, verbose } => sweep(&cat, seconds, filter.as_deref(), verbose),
        Cmd::Pc98 { game, index, seconds, setup_seconds, sound_vector, trace, trace_steps, cmd, trace_capture } => {
            pc98_diag(&cat, &game, index, seconds, setup_seconds, sound_vector.as_deref(), trace, trace_steps, cmd.as_deref(), trace_capture)?
        }
        Cmd::Pc98Rip { game, index, seconds, out, format, headroom_db, min_loop, no_loop, opn_clock_hz, verbose } => {
            pc98_rip(&cat, &game, index, seconds, &out, &format, headroom_db, min_loop, no_loop, opn_clock_hz, verbose)?
        }
        Cmd::Pc98Sweep { seconds, filter, kinds, limit, skip, oneline, verbose } => {
            pc98_sweep(&cat, seconds, filter.as_deref(), &kinds, limit, skip, oneline, verbose)
        }
        Cmd::RipOne { ordinal, out, seconds, format, headroom_db, min_loop, no_loop, deadline } => {
            let sum = rip_one_set(&cat, ordinal, &out, seconds, &format, headroom_db, min_loop, no_loop, deadline);
            println!("{}", sum.to_json());
        }
        Cmd::ArchiveRip {
            out, manifest, platforms, kinds, jobs, seconds, timeout, title_deadline, format, limit, no_resume,
            retry_failed, dry_run, only_archives,
        } => archive_rip(
            &archive_path, &cat, &out, manifest.as_ref(), &platforms, &kinds, jobs, seconds, timeout, title_deadline,
            &format, limit, no_resume, retry_failed, dry_run, only_archives.as_ref(),
        )?,
        // Handled above, before the catalogue load.
        Cmd::Triage { .. } => unreachable!(),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pc98_diag(
    cat: &Catalogue,
    game_query: &str,
    index: usize,
    seconds: f64,
    setup_seconds: f64,
    sound_vector: Option<&str>,
    trace: Option<usize>,
    trace_steps: u64,
    cmd_override: Option<&str>,
    trace_capture: bool,
) -> Result<()> {
    let lc = game_query.to_lowercase();
    let (_, g) = cat
        .games
        .iter()
        .find(|(_, g)| g.driver.platform == "pc98dos" && g.name.to_lowercase().contains(&lc))
        .with_context(|| format!("no pc98dos game matching {game_query:?}"))?;
    let archive = g
        .romlist
        .as_ref()
        .and_then(|r| r.archive.as_deref())
        .context("game has no romlist archive")?;
    let set_dir = cat
        .find_set_dir(archive)
        .with_context(|| format!("set folder {archive:?} not found on disk"))?;

    let titles = g.expanded_titles();
    let t = titles.get(index).with_context(|| format!("no title index {index}"))?;

    let funcvect = g
        .options
        .iter()
        .find(|o| o.name == "funcvect")
        .and_then(|o| hoot_xml::parse_num(&o.value))
        .map(|v| v as u8);
    let forced = sound_vector.and_then(hoot_xml::parse_num).map(|v| v as u8);

    let opts = hoot_machine::pc98::Pc98RipOptions {
        seconds,
        setup_seconds,
        clockmul: clockmul_of(g),
        funcvect,
        sound_vector: forced,
        deadline_secs: None,
        opn_clock_hz: None,
        fm_variant: is_fm_variant(g),
    };

    println!("{}", g.name);
    println!("  title [{index}] {:#06x}  {}", t.code, t.name);
    println!("  driver kind: {}  funcvect: {}", g.driver.kind.as_deref().unwrap_or("-"),
        funcvect.map(|v| format!("{v:#04x}")).unwrap_or_else(|| "-".into()));

    if let Some(cmd_index) = trace {
        let r = hoot_machine::pc98::trace_title(g, &set_dir, t.code, cmd_index, trace_steps, &opts, cmd_override)?;
        println!("\n  trace of shell[{cmd_index}]: {}", r.cmd);
        println!("  stepped {} instructions{}  ({} FM writes)",
            r.steps, if r.stalled { "  [STALLED]" } else { "" }, r.fm_writes);

        println!("\n  hottest PCs (linear addr : hits):");
        for (pc, hits) in r.hot.iter().take(12) {
            println!("    {pc:#08x} : {hits}");
        }

        if !r.int_counts.is_empty() {
            let ic: Vec<String> = r.int_counts.iter().map(|(v, n)| format!("{v:#04x}×{n}")).collect();
            println!("\n  INT vectors serviced: {}", ic.join(" "));
        }
        println!("\n  first INT calls (vector AH @ pc):");
        for (v, ah, pc) in r.int_seq.iter().take(30) {
            println!("    INT {v:#04x} AH={ah:#04x} @ {pc:#08x}");
        }

        if !r.unknown_ports.is_empty() {
            let ports: Vec<String> = r.unknown_ports.iter()
                .map(|(p, (rd, wr))| format!("{p:#06x}(r{rd}/w{wr})")).collect();
            println!("\n  unmodelled ports: {}", ports.join(" "));
        }
        let console = r.console.trim();
        if !console.is_empty() {
            println!("\n  console output:");
            for line in console.lines() {
                println!("    | {line}");
            }
        }
        return Ok(());
    }

    if trace_capture {
        let steps = if trace_steps == 5_000_000 { 3_000_000 } else { trace_steps };
        let r = hoot_machine::pc98::trace_capture(g, &set_dir, t.code, steps, &opts)?;
        println!("\n  shell chain:");
        for (cmd, res) in &r.shell {
            println!("    {cmd:<24} -> {res}");
        }
        println!("\n  capture trace: {} steps, {} FM writes, {} IRQs", r.steps, r.fm_writes, r.irqs);
        println!("\n  DOS reads (handle: bytes):");
        for (h, n) in r.reads.iter().take(20) {
            println!("    handle {h}: {n} bytes");
        }
        println!("\n  driver API calls (INT / AH / AL):");
        if r.api_calls.is_empty() {
            println!("    (none observed)");
        }
        for (v, ah, al) in r.api_calls.iter().take(60) {
            println!("    INT {v:#04x}  AH={ah:#04x} AL={al:#04x}");
        }
        println!("\n  hottest PCs:");
        for (pc, hits) in r.hot.iter().take(12) {
            println!("    {pc:#08x} : {hits}");
        }
        println!("\n  frozen PCs (IRQ pending, interrupts disabled):");
        for (pc, hits) in r.frozen.iter().take(12) {
            println!("    {pc:#08x} : {hits}");
        }
        println!("\n  wild jumps into buffer (from -> to):");
        for (from, to) in r.wild_jumps.iter().take(12) {
            println!("    {from:#08x} -> {to:#08x}");
        }
        return Ok(());
    }

    let o = hoot_machine::pc98::rip_title(g, &set_dir, t.code, &opts)?;

    println!("\n  shell chain:");
    for s in &o.shell {
        println!("    {:<28} -> {:?}  ({} cyc)", s.cmd, s.result, s.cycles);
    }

    println!("\n  hooked IVT vectors:");
    if o.hooked_vectors.is_empty() {
        println!("    (none)");
    }
    for (v, (seg, off)) in &o.hooked_vectors {
        let tag = if Some(*v) == funcvect { "  <- funcvect" } else { "" };
        println!("    INT {v:#04x} -> {seg:#06x}:{off:#06x}{tag}");
    }

    if !o.installed_vectors.is_empty() {
        let iv: Vec<String> = o.installed_vectors.keys().map(|v| format!("{v:#04x}")).collect();
        println!("  (of which set via AH=25h: {})", iv.join(" "));
    }

    println!("\n  capture:");
    println!("    sound vector : {}", o.sound_vector.map(|v| format!("{v:#04x}")).unwrap_or_else(|| "none".into()));
    println!("    opn timer used: {}", o.opn_timer_used);
    println!("    timer IRQs   : {}", o.irqs);
    println!("    FM writes    : {} total, {} captured", o.total_fm_writes, o.captured_writes);
    let (irq, ta, tb) = o.opn_end_state;
    println!("    opn end: irq={irq}  timerA(run={} en={} cnt={} per={} flag={})  timerB(run={} en={} cnt={} per={} flag={})",
        ta.0, ta.1, ta.2, ta.3, ta.4, tb.0, tb.1, tb.2, tb.3, tb.4);
    println!("    undelivered OPN IRQs: if_clear={} no_vector={}", o.dbg_if_clear, o.dbg_no_vec);
    println!("    MCB chain (seg owner size_paras -> end_seg):");
    for (seg, owner, size) in &o.mcb_chain {
        let kind = if *owner == 0 { "free" } else { "used" };
        println!("      {seg:#06x} owner={owner:#06x} {size:#06x}p -> {:#06x}  [{kind}]", seg + 1 + size);
    }

    // Deliberately not gated on audibility: a silent log is exactly what you
    // want dumped for inspection when debugging a set that plays nothing.
    if !o.log.writes.is_empty() {
        use hoot_log::s98::{write_s98, S98Tags};
        use hoot_log::{write_vgz, Gd3};
        let base = std::path::Path::new("pc98_out");
        let mut tags = S98Tags::default();
        tags.set("title", &t.name);
        tags.set("game", &g.name);
        tags.set("system", "NEC PC-9801");
        tags.set("s98by", "hootrip");
        std::fs::write("pc98_out.s98", write_s98(&o.log, &tags)?)?;
        let gd3 = Gd3 {
            track_en: t.name.clone(),
            game_en: g.name.clone(),
            system_en: "NEC PC-9801".into(),
            ripper: "hootrip".into(),
            ..Default::default()
        };
        std::fs::write("pc98_out.vgz", write_vgz(&o.log, &gd3)?)?;
        let _ = base;
        println!("    wrote pc98_out.s98 / pc98_out.vgz ({} writes)", o.log.writes.len());
        let tps = o.log.ticks_per_second as f64;
        let ws = &o.log.writes;
        if let (Some(f), Some(l)) = (ws.first(), ws.last()) {
            println!("    write span: {:.3}s .. {:.3}s  (of {:.1}s capture)",
                f.t as f64 / tps, l.t as f64 / tps, o.log.end_t as f64 / tps);
        }
        let r27 = ws.iter().filter(|w| w.port == 0 && w.addr == 0x27).count();
        let keyons = ws.iter().filter(|w| w.port == 0 && w.addr == 0x28).count();
        println!("    reg 0x27 timer-ctrl writes: {r27}   reg 0x28 key on/off: {keyons}");
        let mut r27vals: std::collections::BTreeMap<u8, u64> = std::collections::BTreeMap::new();
        for w in ws.iter().filter(|w| w.port == 0 && w.addr == 0x27) {
            *r27vals.entry(w.data).or_default() += 1;
        }
        let vs: Vec<String> = r27vals.iter().map(|(v, n)| format!("{v:#04x}×{n}")).collect();
        println!("    reg 0x27 values written: {}", vs.join(" "));
    }

    if !o.unimpl.is_empty() {
        println!("\n  unimplemented DOS/INT calls:");
        for ((int, ah), n) in &o.unimpl {
            println!("    INT {int:#04x} AH={ah:#04x}  x{n}");
        }
    }

    if !o.unknown_ports.is_empty() {
        println!("\n  unmodelled ports:");
        let ports: Vec<String> = o.unknown_ports.iter()
            .map(|(p, (r, w))| format!("{p:#06x}(r{r}/w{w})")).collect();
        println!("    {}", ports.join(" "));
    }

    let console = o.console.trim();
    if !console.is_empty() {
        println!("\n  console output:");
        for line in console.lines() {
            println!("    | {line}");
        }
    }
    Ok(())
}

/// Locate a pc98dos game by name substring and resolve its set folder.
fn find_pc98dos<'a>(
    cat: &'a Catalogue,
    query: &str,
) -> Result<(&'a hoot_xml::Game, PathBuf)> {
    let lc = query.to_lowercase();
    let (_, g) = cat
        .games
        .iter()
        .find(|(_, g)| g.driver.platform == "pc98dos" && g.name.to_lowercase().contains(&lc))
        .with_context(|| format!("no pc98dos game matching {query:?}"))?;
    let archive = g
        .romlist
        .as_ref()
        .and_then(|r| r.archive.as_deref())
        .context("game has no romlist archive")?;
    let set_dir = cat
        .find_set_dir(archive)
        .with_context(|| format!("set folder {archive:?} not found on disk"))?;
    Ok((g, set_dir))
}

#[allow(clippy::too_many_arguments)]
fn pc98_rip(
    cat: &Catalogue,
    game_query: &str,
    index: Option<usize>,
    seconds: f64,
    out: &PathBuf,
    format: &str,
    headroom_db: f32,
    min_loop: f64,
    no_loop: bool,
    opn_clock_hz: Option<u32>,
    verbose: bool,
) -> Result<()> {
    use hoot_log::loops::apply_loop;
    use hoot_log::s98::{write_s98, S98Tags};
    use hoot_log::{vgm_volume_modifier, write_vgz, Gd3};

    let (g, set_dir) = find_pc98dos(cat, game_query)?;

    let funcvect = g
        .options
        .iter()
        .find(|o| o.name == "funcvect")
        .and_then(|o| hoot_xml::parse_num(&o.value))
        .map(|v| v as u8);
    let opts = hoot_machine::pc98::Pc98RipOptions {
        seconds,
        setup_seconds: 3.0,
        clockmul: clockmul_of(g),
        funcvect,
        sound_vector: None,
        deadline_secs: None,
        opn_clock_hz,
        fm_variant: is_fm_variant(g),
    };
    let volmod = vgm_volume_modifier(headroom_db);
    let system = g
        .driver_alias
        .as_ref()
        .and_then(|a| a.kind.clone())
        .unwrap_or_else(|| "NEC PC-9801".into());

    let titles = g.expanded_titles();
    let selected: Vec<(usize, hoot_xml::Title)> = match index {
        Some(i) => vec![(i, titles.get(i).with_context(|| format!("no title index {i}"))?.clone())],
        None => titles.into_iter().enumerate().collect(),
    };

    // FM-variant rips are tagged with the OPN name, not the set's MIDI title.
    let game_name = if opts.fm_variant {
        fm_variant_game_name(&g.name)
    } else {
        g.name.clone()
    };
    let dir = out.join("pc98").join(sanitize(&game_name));
    std::fs::create_dir_all(&dir)?;
    println!("{}\n  {} title(s) → {}  (headroom {:.1}dB, volmod {:#04x})",
        game_name, selected.len(), dir.display(), headroom_db, volmod);

    for (i, t) in selected.iter() {
        let i = *i;
        // When ripping a whole set, skip 演奏停止/[STOP]/無音 pseudo-tracks — they
        // are silent-by-design selectors, not songs (see stop-track note in
        // docs/pc98-driver-families.md). An explicit --index rips them anyway.
        if index.is_none() && is_stop_title(&t.name) {
            println!("  [{i:02}] {:<40} skipped (stop/SE pseudo-track)", t.name);
            continue;
        }
        let track_name = if opts.fm_variant {
            fm_variant_track_name(&t.name)
        } else {
            t.name.clone()
        };
        let mut outcome = hoot_machine::pc98::rip_title(g, &set_dir, t.code, &opts)?;
        let raw_writes = outcome.log.writes.len();
        outcome.log.volume_modifier = volmod;

        // Loop detection (trims to intro + one clean loop) or trailing-silence trim.
        let looped = if no_loop { false } else { apply_loop(&mut outcome.log, min_loop) };
        if !looped {
            outcome.log.trim_trailing(0.5);
        }
        let tps = outcome.log.ticks_per_second as f64;
        let loop_note = match outcome.log.loop_t {
            Some(lt) => format!("loop@{:.1}s len {:.1}s", lt as f64 / tps,
                (outcome.log.end_t - lt) as f64 / tps),
            None => "one-shot".into(),
        };

        let n_writes = outcome.log.writes.len();
        let base = format!("{:02} {}", i, sanitize(&track_name));
        let status = if n_writes == 0 {
            "NO WRITES"
        } else {
            if format == "s98" || format == "both" {
                let mut tags = S98Tags::default();
                tags.set("title", &track_name);
                tags.set("game", &game_name);
                tags.set("system", &system);
                tags.set("s98by", "hootrip");
                std::fs::write(dir.join(format!("{base}.s98")), write_s98(&outcome.log, &tags)?)?;
            }
            if format == "vgz" || format == "both" {
                let gd3 = Gd3 {
                    track_en: track_name.clone(),
                    game_en: game_name.clone(),
                    system_en: system.clone(),
                    ripper: "hootrip".into(),
                    ..Default::default()
                };
                std::fs::write(dir.join(format!("{base}.vgz")), write_vgz(&outcome.log, &gd3)?)?;
            }
            "ok"
        };
        println!("  [{i:02}] {:<40} {:>6} writes  {:>7}s  {:<22} {}",
            track_name, n_writes, format!("{:.1}", outcome.log.end_t as f64 / tps), loop_note, status);
        if verbose && raw_writes != n_writes {
            println!("       captured {raw_writes} writes, trimmed to {n_writes}");
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn pc98_sweep(
    cat: &Catalogue,
    seconds: f64,
    filter: Option<&str>,
    kinds: &str,
    limit: usize,
    skip: usize,
    oneline: bool,
    verbose: bool,
) {
    let filt = filter.map(|f| f.to_lowercase());
    let want: Vec<&str> = kinds.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    let mut matched = 0; // sets matching the filter, before skip
    let mut total = 0;
    let mut ok = 0;
    let mut silent = 0;
    let mut errors = 0;
    let mut timeouts = 0;
    // (ok, opna-extended-used, total) per driver kind.
    let mut by_kind: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    let mut failures: Vec<String> = Vec::new();

    for (_, g) in cat.games.iter().filter(|(_, g)| g.driver.platform == "pc98dos") {
        let kind = g.driver.kind.clone().unwrap_or_else(|| "-".into());
        if !want.iter().any(|k| k.eq_ignore_ascii_case(&kind)) {
            continue;
        }
        if let Some(f) = &filt {
            if !g.name.to_lowercase().contains(f.as_str()) {
                continue;
            }
        }
        let Some(archive) = g.romlist.as_ref().and_then(|r| r.archive.as_deref()) else { continue };
        let Some(set_dir) = cat.find_set_dir(archive) else { continue };
        let Some(t0) = g.expanded_titles().into_iter().next() else { continue };
        // Stable ordinal (independent of skip) so a process-per-set driver can
        // address each set by index across separate invocations.
        let ordinal = matched;
        matched += 1;
        if ordinal < skip {
            continue;
        }
        if limit != 0 && total >= limit {
            break;
        }
        total += 1;
        let ke = by_kind.entry(kind.clone()).or_default();
        ke.2 += 1;

        let funcvect = g
            .options
            .iter()
            .find(|o| o.name == "funcvect")
            .and_then(|o| hoot_xml::parse_num(&o.value))
            .map(|v| v as u8);
        let opts = hoot_machine::pc98::Pc98RipOptions {
            seconds,
            setup_seconds: 2.0,
            clockmul: clockmul_of(g),
            funcvect,
            sound_vector: None,
            // Wall-clock cap: non-working sets spin their whole budget. Bound
            // each so the sweep stays tractable (working sets idle-HLT in <1s).
            deadline_secs: Some(4.0),
            opn_clock_hz: None,
            fm_variant: is_fm_variant(g),
        };
        if std::env::var_os("HOOTRIP_SWEEP_TRACE").is_some() {
            eprintln!("  >> [{ordinal}] {} ({kind})", g.name);
        }
        let (status, ext) = match hoot_machine::pc98::rip_title(g, &set_dir, t0.code, &opts) {
            Ok(o) if !hoot_log::audibility(&o.log).is_silent() => {
                ok += 1;
                ke.0 += 1;
                let ext = o.log.writes.iter().any(|w| w.port == 1);
                if ext {
                    ke.1 += 1; // OPNA extended channels actually driven
                }
                ("ok", ext)
            }
            Ok(o) if o.timed_out => {
                timeouts += 1;
                failures.push(format!("[timeout] {} ({kind})", g.name));
                ("timeout", false)
            }
            Ok(_) => {
                silent += 1;
                failures.push(format!("[silent] {} ({kind})", g.name));
                ("silent", false)
            }
            Err(e) => {
                errors += 1;
                failures.push(format!("[error] {} ({kind}): {e}", g.name));
                ("error", false)
            }
        };
        if oneline {
            println!("{status}\t{kind}\t{}\t{}", ext as u8, g.name);
        }
        if !oneline && total % 50 == 0 {
            eprintln!("  ...{total} swept ({ok} ok, {timeouts} timeout)");
        }
    }

    if oneline {
        return; // per-set lines already emitted; no summary
    }
    println!("pc98 sweep [{kinds}]: {total} sets, {ok} ok, {silent} silent, {timeouts} timeout, {errors} errored");
    println!("by driver kind (ok / opna-ext / total):");
    for (k, (o, ext, t)) in &by_kind {
        println!("  {k:<8} {o:>4} / {ext:>4} / {t}");
    }
    if verbose && !failures.is_empty() {
        println!("\nfailures:");
        for f in &failures {
            println!("  {f}");
        }
    } else if !failures.is_empty() {
        println!("({} silent/errored; pass --verbose to list)", failures.len());
    }
}

fn sweep(cat: &Catalogue, seconds: f64, filter: Option<&str>, verbose: bool) {
    let filt = filter.map(|f| f.to_lowercase());
    let mut total = 0;
    let mut ok = 0;
    let mut no_writes = 0;
    let mut errors = 0;
    // Bucket outcomes by driver kind so PC-98 planning can see which driver
    // families are already solid and which need work.
    let mut by_kind: BTreeMap<String, (usize, usize)> = BTreeMap::new(); // (ok, total)
    let mut failures: Vec<String> = Vec::new();

    for (_, g) in cat.games.iter().filter(|(_, g)| g.driver.platform == "pc88") {
        if let Some(f) = &filt {
            if !g.name.to_lowercase().contains(f.as_str()) {
                continue;
            }
        }
        let Some(archive) = g.romlist.as_ref().and_then(|r| r.archive.as_deref()) else {
            continue;
        };
        let Some(set_dir) = cat.find_set_dir(archive) else {
            continue;
        };
        let Some(t0) = g.expanded_titles().into_iter().next() else {
            continue;
        };
        total += 1;
        let kind = g.driver.kind.clone().unwrap_or_else(|| "-".into());
        let ke = by_kind.entry(kind.clone()).or_default();
        ke.1 += 1;

        let opts = hoot_machine::RipOptions { seconds, clockmul: clockmul_of(g), ..Default::default() };
        match hoot_machine::rip_title(g, &set_dir, t0.code, &opts) {
            Ok(o) if !hoot_log::audibility(&o.log).is_silent() => {
                ok += 1;
                ke.0 += 1;
            }
            Ok(_) => {
                no_writes += 1;
                failures.push(format!("[no writes] {} ({kind})", g.name));
            }
            Err(e) => {
                errors += 1;
                failures.push(format!("[error] {} ({kind}): {e}", g.name));
            }
        }
    }

    println!("pc88 sweep: {total} sets, {ok} ok, {no_writes} silent, {errors} errored");
    println!("by driver kind (ok/total):");
    for (k, (o, t)) in &by_kind {
        println!("  {k:<10} {o:>4}/{t}");
    }
    if verbose && !failures.is_empty() {
        println!("\nfailures:");
        for f in &failures {
            println!("  {f}");
        }
    } else if !failures.is_empty() {
        println!("({} failures; pass --verbose to list)", failures.len());
    }
}

fn find_pc88<'a>(cat: &'a Catalogue, query: &str) -> Result<(&'a hoot_xml::Game, PathBuf)> {
    let lc = query.to_lowercase();
    let (_, g) = cat
        .games
        .iter()
        .find(|(_, g)| g.name.to_lowercase().contains(&lc))
        .with_context(|| format!("no game matching {query:?}"))?;
    if g.driver.platform != "pc88" {
        anyhow::bail!("only pc88 sets are supported so far; {} is {}", g.name, g.driver.platform);
    }
    let archive = g
        .romlist
        .as_ref()
        .and_then(|r| r.archive.as_deref())
        .context("game has no romlist archive")?;
    let set_dir = cat
        .find_set_dir(archive)
        .with_context(|| format!("set folder {archive:?} not found on disk"))?;
    Ok((g, set_dir))
}

fn clockmul_of(g: &hoot_xml::Game) -> u32 {
    g.options
        .iter()
        .find(|o| o.name == "clockmul" || o.name == "clock_mul")
        .and_then(|o| hoot_xml::parse_num(&o.value))
        .filter(|&v| v > 0)
        .unwrap_or(1) as u32
}

/// A Vermouth/TGLFMP2 set renders its melody as MIDI (`midiout=1`), leaving the
/// OPN silent; its original FM soundtrack ships alongside as `.MFM`. Ripping such
/// a set in FM-variant mode captures that FM music instead of silent MIDI churn.
fn is_fm_variant(g: &hoot_xml::Game) -> bool {
    g.options.iter().any(|o| o.name == "midiout")
}

/// The game name to tag an FM-variant rip with: strip the MIDI-device and
/// Vermouth/GUS-simulation markers (this is the OPN soundtrack, not the SC-55/GS
/// MIDI arrangement the set nominally plays) and mark it `(OPN)`.
fn fm_variant_game_name(name: &str) -> String {
    let mut s = name.to_string();
    for pat in [
        " (Vermouth, Gravis Ultrasound simulation)",
        " (SC-55)", " (SC-88)", " (GS)", " (GM)", " (LA)", " (CM-64)", " (MT-32)", " (OPN)",
    ] {
        s = s.replace(pat, "");
    }
    format!("{} (OPN)", s.trim())
}

/// Clean a Vermouth track title for an FM-variant rip: the set names each track
/// after its `.MGS` MIDI file (e.g. "HBM062.MGS : 与えるもの (Sweethearts)"); drop
/// that file-name prefix so the tag is just the song title.
fn fm_variant_track_name(name: &str) -> String {
    if let Some((head, tail)) = name.split_once(" : ") {
        let h = head.trim();
        let is_song_file = h
            .rsplit_once('.')
            .is_some_and(|(_, ext)| matches!(ext.to_ascii_uppercase().as_str(), "MGS" | "MG2" | "MFM" | "MF2"));
        if is_song_file {
            return tail.trim().to_string();
        }
    }
    name.to_string()
}

fn compare_cmd(
    cat: &Catalogue,
    game_query: &str,
    index: usize,
    reference: &PathBuf,
    seconds: Option<f64>,
) -> Result<()> {
    use hoot_log::s98::{write_s98, S98Tags};
    use hoot_log::{compare, read_s98};

    let ref_bytes = std::fs::read(reference)
        .with_context(|| format!("reading reference {}", reference.display()))?;
    let ref_parsed = read_s98(&ref_bytes).context("parsing reference S98")?;
    let ref_len_s = ref_parsed
        .events
        .last()
        .map(|(t, ..)| *t as f64 * ref_parsed.sync_secs)
        .unwrap_or(0.0);

    let (g, set_dir) = find_pc88(cat, game_query)?;
    let titles = g.expanded_titles();
    let t = titles.get(index).with_context(|| format!("no title index {index}"))?;

    let secs = seconds.unwrap_or(ref_len_s.max(1.0));
    let opts = hoot_machine::RipOptions { seconds: secs, clockmul: clockmul_of(g), ..Default::default() };
    let outcome = hoot_machine::rip_title(g, &set_dir, t.code, &opts)?;
    let ours = read_s98(&write_s98(&outcome.log, &S98Tags::default())?)?;

    let r = compare(&ours, &ref_parsed);
    println!("Comparing rip of {:?} [{}] against {}", g.name, t.name, reference.display());
    println!("  reference length : {ref_len_s:.2}s ({} writes)", r.ref_writes);
    println!("  our rip          : {secs:.2}s ({} writes)", r.ours_writes);
    println!("  longest common   : {} writes ({:.1}% of reference)", r.lcs,
        100.0 * r.lcs as f64 / r.ref_writes.max(1) as f64);
    println!("  tempo ratio      : {:.4}  (ref/ours; 1.0 = identical tempo)", r.tempo_ratio);
    println!("  timing error     : mean {:.2}ms  median {:.2}ms (after tempo-normalise)",
        r.mean_abs_dt_ms, r.median_abs_dt_ms);
    if !r.divergent_addrs.is_empty() {
        let d: Vec<String> = r.divergent_addrs.iter()
            .map(|(a, c)| format!("{a:#04x}(Δ{c})")).collect();
        println!("  divergent regs   : {}", d.join(" "));
    }
    Ok(())
}

fn rip(
    cat: &Catalogue,
    game_query: &str,
    index: Option<usize>,
    seconds: f64,
    out: &PathBuf,
    format: &str,
    verbose: bool,
) -> Result<()> {
    use hoot_log::s98::{write_s98, S98Tags};
    use hoot_log::{write_vgz, Gd3};

    let (g, set_dir) = find_pc88(cat, game_query)?;
    let opts = hoot_machine::RipOptions {
        seconds,
        clockmul: clockmul_of(g),
        ..Default::default()
    };

    let titles = g.expanded_titles();
    // Keep each title's real index so filenames are stable whether we rip one
    // title or all of them.
    let selected: Vec<(usize, hoot_xml::Title)> = match index {
        Some(i) => vec![(i, titles.get(i).with_context(|| format!("no title index {i}"))?.clone())],
        None => titles.into_iter().enumerate().collect(),
    };

    std::fs::create_dir_all(out)?;
    println!("{} — {} title(s), {}s each", g.name, selected.len(), seconds);

    for (i, t) in selected.iter() {
        let i = *i;
        let outcome = hoot_machine::rip_title(g, &set_dir, t.code, &opts)?;
        let n_writes = outcome.log.writes.len();

        let system = g
            .driver_alias
            .as_ref()
            .and_then(|a| a.kind.clone())
            .unwrap_or_else(|| "NEC PC-8801".into());
        let base = format!("{:02} {}", i, sanitize(&t.name));

        let wrote = if n_writes > 0 {
            if format == "s98" || format == "both" {
                let mut tags = S98Tags::default();
                tags.set("title", &t.name);
                tags.set("game", &g.name);
                tags.set("system", &system);
                tags.set("s98by", "hootrip");
                std::fs::write(out.join(format!("{base}.s98")), write_s98(&outcome.log, &tags)?)?;
            }
            if format == "vgz" || format == "both" {
                let gd3 = Gd3 {
                    track_en: t.name.clone(),
                    game_en: g.name.clone(),
                    system_en: system.clone(),
                    ripper: "hootrip".into(),
                    ..Default::default()
                };
                std::fs::write(out.join(format!("{base}.vgz")), write_vgz(&outcome.log, &gd3)?)?;
            }
            "ok"
        } else {
            "NO WRITES"
        };
        println!(
            "  [{i:02}] {:<40} {:>7} writes  rtc/opn irqs {}/{}  {}",
            t.name, n_writes, outcome.irqs.0, outcome.irqs.1, wrote
        );
        if verbose {
            if !outcome.unknown_ports.is_empty() {
                let ports: Vec<String> = outcome
                    .unknown_ports
                    .iter()
                    .map(|(p, (r, w))| format!("{p:#04x}(r{r}/w{w})"))
                    .collect();
                println!("       unmodelled ports: {}", ports.join(" "));
            }
            let vecs: Vec<String> = outcome
                .vectors
                .iter()
                .enumerate()
                .filter(|(_, v)| **v != 0)
                .map(|(l, v)| format!("L{l}→{v:#06x}"))
                .collect();
            println!("       im2 vectors: {}", vecs.join(" "));
        }
    }
    Ok(())
}

/// A title whose label marks it as a stop / silence / sound-effect selector
/// rather than a song. 146 pc98dos sets carry such a pseudo-track at index 0
/// (「演奏停止」 / [STOP] / 無音 / SND OFF); ripping it produces a silent file. The
/// archive rip skips these but still counts them (`stop_skipped`).
fn is_stop_title(name: &str) -> bool {
    let n = name.trim();
    if n.contains('停') || n.contains("停止") || n.contains("無音") {
        return true;
    }
    let up = n.to_ascii_uppercase();
    up == "OFF"
        || up.contains("[STOP]")
        || up.contains("SND OFF")
        || up.contains("SOUND OFF")
        || up.contains("SND_OFF")
}

/// One census record per set, emitted by `rip-one` and aggregated by
/// `archive-rip`. Kept as a hand-rolled JSON line to avoid a serde dependency.
struct SetSummary {
    ordinal: usize,
    platform: String,
    kind: String,
    name: String,
    archive: String,
    /// ok | partial | silent | stoponly | timeout | error | nofolder | unsupported
    status: String,
    titles: usize,
    ripped: usize,
    /// Titles dropped because nothing was ever keyed on (dead log).
    silent_titles: usize,
    /// Titles dropped because notes played but no voice was ever programmed.
    novoice_titles: usize,
    /// Titles dropped because their only content is ADPCM, which neither
    /// writer can currently represent (a format gap, not a failed rip).
    adpcm_titles: usize,
    stop_skipped: usize,
    writes: usize,
    keyons: usize,
    looped: usize,
    err: String,
}

impl SetSummary {
    fn to_json(&self) -> String {
        format!(
            "{{\"ordinal\":{},\"platform\":\"{}\",\"kind\":\"{}\",\"name\":\"{}\",\"archive\":\"{}\",\
\"status\":\"{}\",\"titles\":{},\"ripped\":{},\"silent\":{},\"novoice\":{},\"adpcm\":{},\"stop_skipped\":{},\"writes\":{},\
\"keyons\":{},\"looped\":{},\"err\":\"{}\"}}",
            self.ordinal,
            json_escape(&self.platform),
            json_escape(&self.kind),
            json_escape(&self.name),
            json_escape(&self.archive),
            self.status,
            self.titles,
            self.ripped,
            self.silent_titles,
            self.novoice_titles,
            self.adpcm_titles,
            self.stop_skipped,
            self.writes,
            self.keyons,
            self.looped,
            json_escape(&self.err),
        )
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// Extract a numeric JSON field value from a flat one-line record.
fn json_num(line: &str, key: &str) -> Option<usize> {
    let pat = format!("\"{key}\":");
    let i = line.find(&pat)? + pat.len();
    let rest = &line[i..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest.get(..end)?.parse().ok()
}

/// Extract a string JSON field value from a flat one-line record.
fn json_str(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let i = line.find(&pat)? + pat.len();
    let mut out = String::new();
    let mut esc = false;
    for c in line[i..].chars() {
        if esc {
            out.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == '"' {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// Write one song's log to the requested format(s) under `dir/base.{s98,vgz}`.
fn write_track(
    dir: &std::path::Path,
    base: &str,
    log: &hoot_log::RegisterLog,
    track: &str,
    game: &str,
    system: &str,
    format: &str,
) -> Result<()> {
    use hoot_log::s98::{write_s98, S98Tags};
    use hoot_log::{write_vgz, Gd3};
    if format == "s98" || format == "both" {
        let mut tags = S98Tags::default();
        tags.set("title", track);
        tags.set("game", game);
        tags.set("system", system);
        tags.set("s98by", "hootrip");
        std::fs::write(dir.join(format!("{base}.s98")), write_s98(log, &tags)?)?;
    }
    if format == "vgz" || format == "both" {
        let gd3 = Gd3 {
            track_en: track.into(),
            game_en: game.into(),
            system_en: system.into(),
            ripper: "hootrip".into(),
            ..Default::default()
        };
        std::fs::write(dir.join(format!("{base}.vgz")), write_vgz(log, &gd3)?)?;
    }
    Ok(())
}

fn finalize_status(sum: &mut SetSummary, any_timeout: bool) {
    if !sum.err.is_empty() {
        sum.status = "error".into();
        return;
    }
    let effective = sum.titles.saturating_sub(sum.stop_skipped);
    sum.status = if sum.ripped == 0 {
        if any_timeout {
            "timeout"
        } else if effective == 0 {
            "stoponly"
        } else {
            "silent"
        }
    } else if sum.silent_titles > 0
        || sum.novoice_titles > 0
        || sum.adpcm_titles > 0
        || any_timeout
    {
        "partial"
    } else {
        "ok"
    }
    .into();
}

/// Rip a single catalogue entry (all non-stop titles) and return a census
/// record. Pure work unit: writes song files, prints nothing.
#[allow(clippy::too_many_arguments)]
fn rip_one_set(
    cat: &Catalogue,
    ordinal: usize,
    out: &std::path::Path,
    seconds: f64,
    format: &str,
    headroom_db: f32,
    min_loop: f64,
    no_loop: bool,
    deadline: Option<f64>,
) -> SetSummary {
    use hoot_log::loops::apply_loop;
    use hoot_log::vgm_volume_modifier;

    let Some((_, g)) = cat.games.get(ordinal) else {
        return SetSummary {
            ordinal,
            platform: String::new(),
            kind: String::new(),
            name: String::new(),
            archive: String::new(),
            status: "error".into(),
            titles: 0,
            ripped: 0,
            silent_titles: 0,
            novoice_titles: 0,
            adpcm_titles: 0,
            stop_skipped: 0,
            writes: 0,
            keyons: 0,
            looped: 0,
            err: format!("ordinal {ordinal} out of range"),
        };
    };

    let mut sum = SetSummary {
        ordinal,
        platform: g.driver.platform.clone(),
        kind: g.driver.kind.clone().unwrap_or_else(|| "-".into()),
        name: g.name.clone(),
        archive: g.romlist.as_ref().and_then(|r| r.archive.clone()).unwrap_or_default(),
        status: "error".into(),
        titles: 0,
        ripped: 0,
        silent_titles: 0,
        novoice_titles: 0,
        adpcm_titles: 0,
        stop_skipped: 0,
        writes: 0,
        keyons: 0,
        looped: 0,
        err: String::new(),
    };

    let Some(set_dir) = g
        .romlist
        .as_ref()
        .and_then(|r| r.archive.as_deref())
        .and_then(|a| cat.find_set_dir(a))
    else {
        sum.status = "nofolder".into();
        return sum;
    };

    let titles = g.expanded_titles();
    sum.titles = titles.len();

    let volmod = vgm_volume_modifier(headroom_db);
    let apply_out = |log: &mut hoot_log::RegisterLog| {
        log.volume_modifier = volmod;
        let looped = if no_loop { false } else { apply_loop(log, min_loop) };
        if !looped {
            log.trim_trailing(0.5);
        }
    };
    let count_keyons =
        |log: &hoot_log::RegisterLog| log.writes.iter().filter(|w| w.port == 0 && w.addr == 0x28).count();

    match sum.platform.as_str() {
        "pc98dos" => {
            let funcvect = g
                .options
                .iter()
                .find(|o| o.name == "funcvect")
                .and_then(|o| hoot_xml::parse_num(&o.value))
                .map(|v| v as u8);
            let opts = hoot_machine::pc98::Pc98RipOptions {
                seconds,
                setup_seconds: 3.0,
                clockmul: clockmul_of(g),
                funcvect,
                sound_vector: None,
                deadline_secs: deadline,
                opn_clock_hz: None,
                fm_variant: is_fm_variant(g),
            };
            let system = g
                .driver_alias
                .as_ref()
                .and_then(|a| a.kind.clone())
                .unwrap_or_else(|| "NEC PC-9801".into());
            let game_name = if opts.fm_variant { fm_variant_game_name(&g.name) } else { g.name.clone() };
            let dir = out.join("pc98").join(sanitize(&game_name));
            if let Err(e) = std::fs::create_dir_all(&dir) {
                sum.err = e.to_string();
                finalize_status(&mut sum, false);
                return sum;
            }
            let mut any_timeout = false;
            for (i, t) in titles.iter().enumerate() {
                if is_stop_title(&t.name) {
                    sum.stop_skipped += 1;
                    continue;
                }
                let track_name = if opts.fm_variant { fm_variant_track_name(&t.name) } else { t.name.clone() };
                let mut outcome = match hoot_machine::pc98::rip_title(g, &set_dir, t.code, &opts) {
                    Ok(o) => o,
                    Err(e) => {
                        sum.err = e.to_string();
                        break;
                    }
                };
                if outcome.timed_out {
                    any_timeout = true;
                }
                apply_out(&mut outcome.log);
                // A log full of chip-init churn that never keys a note is a
                // perfectly well-formed file that renders to digital silence;
                // classify on audible activity, not on write count.
                match hoot_log::audibility(&outcome.log) {
                    hoot_log::Audibility::Dead => {
                        sum.silent_titles += 1;
                        continue;
                    }
                    hoot_log::Audibility::NoVoice => {
                        sum.novoice_titles += 1;
                        continue;
                    }
                    hoot_log::Audibility::AdpcmOnly => {
                        sum.adpcm_titles += 1;
                        continue;
                    }
                    hoot_log::Audibility::Audible => {}
                }
                sum.writes += outcome.log.writes.len();
                sum.keyons += count_keyons(&outcome.log);
                if outcome.log.loop_t.is_some() {
                    sum.looped += 1;
                }
                let base = format!("{:02} {}", i, sanitize(&track_name));
                if let Err(e) = write_track(&dir, &base, &outcome.log, &track_name, &game_name, &system, format) {
                    sum.err = e.to_string();
                    break;
                }
                sum.ripped += 1;
            }
            finalize_status(&mut sum, any_timeout);
        }
        "pc88" => {
            let opts = hoot_machine::RipOptions { seconds, clockmul: clockmul_of(g), ..Default::default() };
            let system = g
                .driver_alias
                .as_ref()
                .and_then(|a| a.kind.clone())
                .unwrap_or_else(|| "NEC PC-8801".into());
            let dir = out.join("pc88").join(sanitize(&g.name));
            if let Err(e) = std::fs::create_dir_all(&dir) {
                sum.err = e.to_string();
                finalize_status(&mut sum, false);
                return sum;
            }
            for (i, t) in titles.iter().enumerate() {
                if is_stop_title(&t.name) {
                    sum.stop_skipped += 1;
                    continue;
                }
                let mut outcome = match hoot_machine::rip_title(g, &set_dir, t.code, &opts) {
                    Ok(o) => o,
                    Err(e) => {
                        sum.err = e.to_string();
                        break;
                    }
                };
                apply_out(&mut outcome.log);
                // A log full of chip-init churn that never keys a note is a
                // perfectly well-formed file that renders to digital silence;
                // classify on audible activity, not on write count.
                match hoot_log::audibility(&outcome.log) {
                    hoot_log::Audibility::Dead => {
                        sum.silent_titles += 1;
                        continue;
                    }
                    hoot_log::Audibility::NoVoice => {
                        sum.novoice_titles += 1;
                        continue;
                    }
                    hoot_log::Audibility::AdpcmOnly => {
                        sum.adpcm_titles += 1;
                        continue;
                    }
                    hoot_log::Audibility::Audible => {}
                }
                sum.writes += outcome.log.writes.len();
                sum.keyons += count_keyons(&outcome.log);
                if outcome.log.loop_t.is_some() {
                    sum.looped += 1;
                }
                let base = format!("{:02} {}", i, sanitize(&t.name));
                if let Err(e) = write_track(&dir, &base, &outcome.log, &t.name, &g.name, &system, format) {
                    sum.err = e.to_string();
                    break;
                }
                sum.ripped += 1;
            }
            finalize_status(&mut sum, false);
        }
        _ => sum.status = "unsupported".into(),
    }
    sum
}

/// Run a child process to completion or until `timeout`, capturing its stdout.
/// Returns (finished_normally, stdout). On timeout the child is killed and
/// `finished` is false.
fn run_child_capture(mut child: std::process::Child, timeout: std::time::Duration) -> (bool, String) {
    use std::io::Read;
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(o) = stdout.as_mut() {
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let start = std::time::Instant::now();
    let finished = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(_) => break false,
        }
    };
    let out = reader.join().unwrap_or_default();
    (finished, out)
}

/// A target set for the orchestrator, carrying enough identity to synthesize a
/// failure record if its child never emits JSON.
#[derive(Clone)]
struct Tgt {
    ord: usize,
    platform: String,
    kind: String,
    name: String,
    archive: String,
    /// Expanded song count — sizes the per-set wall budget so a large but
    /// perfectly healthy set (hundreds of songs) isn't killed as if it hung.
    titles: usize,
}

fn synth_json(t: &Tgt, status: &str, err: &str) -> String {
    SetSummary {
        ordinal: t.ord,
        platform: t.platform.clone(),
        kind: t.kind.clone(),
        name: t.name.clone(),
        archive: t.archive.clone(),
        status: status.into(),
        titles: 0,
        ripped: 0,
        silent_titles: 0,
        novoice_titles: 0,
        adpcm_titles: 0,
        stop_skipped: 0,
        writes: 0,
        keyons: 0,
        looped: 0,
        err: err.into(),
    }
    .to_json()
}

#[allow(clippy::too_many_arguments)]
fn archive_rip(
    archive_path: &std::path::Path,
    cat: &Catalogue,
    out: &std::path::Path,
    manifest: Option<&PathBuf>,
    platforms: &str,
    kinds: &str,
    jobs: usize,
    seconds: f64,
    timeout: f64,
    title_deadline: f64,
    format: &str,
    limit: usize,
    no_resume: bool,
    retry_failed: bool,
    dry_run: bool,
    only_archives: Option<&PathBuf>,
) -> Result<()> {
    use std::collections::{HashMap, HashSet};
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc};

    let plats: HashSet<String> =
        platforms.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
    let want_kinds: HashSet<String> =
        kinds.split(',').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();

    // Optional allow-list of set-folder names (for targeted re-rips / smoketests).
    let only: Option<HashSet<String>> = match only_archives {
        Some(path) => {
            let data = std::fs::read_to_string(path)
                .with_context(|| format!("reading --only-archives {}", path.display()))?;
            let set: HashSet<String> = data
                .lines()
                .map(|l| l.split('#').next().unwrap_or("").trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .collect();
            Some(set)
        }
        None => None,
    };

    // Enumerate targets in catalogue order (== the order `rip-one --ordinal`
    // reproduces from the same archive, so ordinals are stable across runs).
    let mut targets: Vec<Tgt> = Vec::new();
    let mut nofolder = 0usize;
    for (i, (_, g)) in cat.games.iter().enumerate() {
        if !plats.contains(&g.driver.platform.to_lowercase()) {
            continue;
        }
        let kind = g.driver.kind.clone().unwrap_or_else(|| "-".into()).to_lowercase();
        if !want_kinds.contains(&kind) {
            continue;
        }
        let archive_name = g.romlist.as_ref().and_then(|r| r.archive.as_deref());
        if let Some(allow) = &only {
            match archive_name {
                Some(a) if allow.contains(&a.to_lowercase()) => {}
                _ => continue,
            }
        }
        let has_dir = archive_name.and_then(|a| cat.find_set_dir(a)).is_some();
        if !has_dir {
            nofolder += 1;
            continue;
        }
        targets.push(Tgt {
            ord: i,
            platform: g.driver.platform.clone(),
            kind,
            name: g.name.clone(),
            archive: g.romlist.as_ref().and_then(|r| r.archive.clone()).unwrap_or_default(),
            titles: g.expanded_titles().len(),
        });
    }

    let manifest_path = manifest.cloned().unwrap_or_else(|| out.join("manifest.jsonl"));

    // Resume: drop sets already recorded, unless --no-resume. With
    // --retry-failed, previous error/timeout records are re-run.
    let mut done: HashMap<usize, String> = HashMap::new();
    if !no_resume && manifest_path.exists() {
        let data = std::fs::read_to_string(&manifest_path).unwrap_or_default();
        for line in data.lines() {
            if let Some(o) = json_num(line, "ordinal") {
                done.insert(o, json_str(line, "status").unwrap_or_default());
            }
        }
    }
    let before = targets.len();
    targets.retain(|t| match done.get(&t.ord) {
        None => true,
        Some(st) => retry_failed && (st == "error" || st == "timeout"),
    });
    let resumed = before - targets.len();
    if limit != 0 && targets.len() > limit {
        targets.truncate(limit);
    }

    let jobs = if jobs == 0 {
        std::thread::available_parallelism().map(|n| n.get().min(8)).unwrap_or(4)
    } else {
        jobs
    };

    // Plan breakdown by platform/kind.
    let mut plan: BTreeMap<String, usize> = BTreeMap::new();
    for t in &targets {
        *plan.entry(format!("{}/{}", t.platform, t.kind)).or_default() += 1;
    }
    eprintln!(
        "archive-rip: {} sets to rip ({} already in manifest, {} skipped [no set folder]); \
{jobs} workers, {seconds:.0}s capture, {title_deadline:.0}s/song guard, {timeout:.0}s/set ceiling, format={format}",
        targets.len(),
        resumed,
        nofolder,
    );
    for (k, n) in &plan {
        eprintln!("  {k:<16} {n}");
    }
    eprintln!("  manifest: {}", manifest_path.display());
    if dry_run {
        eprintln!("(dry run — nothing ripped)");
        return Ok(());
    }
    if targets.is_empty() {
        eprintln!("nothing to do.");
        return Ok(());
    }

    std::fs::create_dir_all(out)?;
    if let Some(parent) = manifest_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut mf = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&manifest_path)
        .with_context(|| format!("opening manifest {}", manifest_path.display()))?;

    let exe = std::env::current_exe().context("locating hootrip executable")?;
    let n = targets.len();
    let targets = Arc::new(targets);
    let cursor = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::channel::<(usize, String, String)>();

    let mut handles = Vec::new();
    for _ in 0..jobs {
        let targets = Arc::clone(&targets);
        let cursor = Arc::clone(&cursor);
        let tx = tx.clone();
        let exe = exe.clone();
        let archive_path = archive_path.to_path_buf();
        let out = out.to_path_buf();
        let format = format.to_string();
        handles.push(std::thread::spawn(move || loop {
            let idx = cursor.fetch_add(1, Ordering::SeqCst);
            if idx >= targets.len() {
                break;
            }
            let t = &targets[idx];
            // Per-set wall budget: room for every song to hit its spin-guard,
            // capped by the absolute ceiling so a hung set can't stall a worker.
            // +30s covers driver setup and the child's own startup.
            let grace_secs = ((t.titles as f64) * title_deadline + 30.0).min(timeout);
            let grace = std::time::Duration::from_secs_f64(grace_secs + 15.0);
            let mut cmd = Command::new(&exe);
            cmd.arg("--archive")
                .arg(&archive_path)
                .arg("rip-one")
                .arg("--ordinal")
                .arg(t.ord.to_string())
                .arg("--out")
                .arg(&out)
                .arg("--seconds")
                .arg(format!("{seconds}"))
                .arg("--format")
                .arg(&format)
                .arg("--deadline")
                .arg(format!("{title_deadline}"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let (line, status) = match cmd.spawn() {
                Ok(child) => {
                    let (finished, stdout) = run_child_capture(child, grace);
                    let json = stdout.lines().rev().find(|l| l.trim_start().starts_with('{')).map(str::to_string);
                    if !finished {
                        (synth_json(t, "timeout", "wall-clock timeout (killed)"), "timeout".to_string())
                    } else if let Some(l) = json {
                        let st = json_str(&l, "status").unwrap_or_else(|| "error".into());
                        (l, st)
                    } else {
                        (synth_json(t, "error", "child produced no JSON (crash?)"), "error".to_string())
                    }
                }
                Err(e) => (synth_json(t, "error", &format!("spawn failed: {e}")), "error".to_string()),
            };
            if tx.send((t.ord, line, status)).is_err() {
                break;
            }
        }));
    }
    drop(tx);

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut processed = 0usize;
    for (ord, line, status) in rx {
        writeln!(mf, "{line}")?;
        mf.flush().ok();
        *counts.entry(status.clone()).or_default() += 1;
        processed += 1;
        let name = json_str(&line, "name").unwrap_or_default();
        eprintln!("[{processed}/{n}] #{ord} {status:<8} {name}");
    }
    for h in handles {
        let _ = h.join();
    }

    eprintln!("\narchive-rip done: {processed} sets this run");
    for (st, c) in &counts {
        eprintln!("  {st:<10} {c}");
    }
    eprintln!("manifest: {}", manifest_path.display());
    Ok(())
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn stats(cat: &Catalogue) {
    println!("hoot archive: {}", cat.root.display());
    println!();

    if !cat.errors.is_empty() {
        println!("!! {} gamelist file(s) failed to parse:", cat.errors.len());
        for (path, err) in &cat.errors {
            println!("   {}: {}", path.display(), err);
        }
        println!();
    }

    let mut by_platform: BTreeMap<&str, (usize, usize)> = BTreeMap::new(); // games, titles
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut with_archive = 0usize;
    let mut archive_found = 0usize;
    for (_, g) in &cat.games {
        let e = by_platform.entry(g.driver.platform.as_str()).or_default();
        e.0 += 1;
        e.1 += g.expanded_titles().len();
        *by_kind
            .entry(format!(
                "{}/{}",
                g.driver.platform,
                g.driver.kind.as_deref().unwrap_or("-")
            ))
            .or_default() += 1;
        if let Some(archive) = g.romlist.as_ref().and_then(|r| r.archive.as_deref()) {
            with_archive += 1;
            if cat.find_set_dir(archive).is_some() {
                archive_found += 1;
            }
        }
    }

    println!(
        "{} <game> entries, {} bind rules, {} platform data dirs",
        cat.games.len(),
        cat.binds.len(),
        cat.set_dirs.len()
    );
    println!(
        "{} games name a romlist archive; {} of those folders exist on disk",
        with_archive, archive_found
    );
    println!();

    println!("games / songs by driver platform:");
    let mut plat: Vec<_> = by_platform.iter().collect();
    plat.sort_by_key(|(_, (games, _))| std::cmp::Reverse(*games));
    for (p, (games, titles)) in plat {
        println!("  {p:<12} {games:>5} games {titles:>7} songs");
    }
    println!();

    println!("top driver kinds (platform/type):");
    let mut kinds: Vec<_> = by_kind.iter().collect();
    kinds.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (k, n) in kinds.iter().take(25) {
        println!("  {k:<28} {n:>5}");
    }
    println!();

    println!("set folders on disk (per data dir):");
    for (dir, sets) in &cat.set_dirs {
        println!("  {dir:<10} {:>5}", sets.len());
    }
}

fn list(cat: &Catalogue, platform: Option<&str>, name: Option<&str>) {
    let name_lc = name.map(|n| n.to_lowercase());
    // The leading number is the catalogue ordinal — the address `archive-rip`
    // and `rip-one --ordinal` use to target a single set.
    for (i, (src, g)) in cat.games.iter().enumerate() {
        if let Some(p) = platform {
            if !g.driver.platform.eq_ignore_ascii_case(p) {
                continue;
            }
        }
        if let Some(n) = &name_lc {
            if !g.name.to_lowercase().contains(n.as_str()) {
                continue;
            }
        }
        println!(
            "{:>5}  {:<58} {:<10} {:<8} {} [{}]",
            i,
            g.name,
            g.driver.platform,
            g.driver.kind.as_deref().unwrap_or("-"),
            g.expanded_titles().len(),
            src.display()
        );
    }
}

fn show(cat: &Catalogue, name: &str) {
    let lc = name.to_lowercase();
    let Some((src, g)) = cat
        .games
        .iter()
        .find(|(_, g)| g.name.to_lowercase().contains(&lc))
    else {
        eprintln!("no game matching {name:?}");
        std::process::exit(1);
    };
    println!("name:     {}", g.name);
    println!("source:   {}", src.display());
    println!(
        "driver:   {} (type {})",
        g.driver.platform,
        g.driver.kind.as_deref().unwrap_or("-")
    );
    if let Some(a) = &g.driver_alias {
        println!(
            "alias:    {} ({})",
            a.label,
            a.kind.as_deref().unwrap_or("-")
        );
    }
    for o in &g.options {
        println!("option:   {} = {}", o.name, o.value);
    }
    if let Some(rl) = &g.romlist {
        let dir = rl.archive.as_deref().map(|a| {
            cat.find_set_dir(a)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| format!("{a} (NOT FOUND on disk)"))
        });
        println!("archive:  {}", dir.as_deref().unwrap_or("-"));
        for r in &rl.roms {
            println!(
                "rom:      {:<6} offset={:<10} crc32={:<10} {}",
                r.kind,
                r.offset.map(|o| format!("{o:#x}")).unwrap_or_default(),
                r.crc32.map(|c| format!("{c:08x}")).unwrap_or_default(),
                r.name
            );
        }
    }
    let titles = g.expanded_titles();
    println!("titles:   {}", titles.len());
    for t in titles.iter().take(50) {
        println!("  {:#010x}  {}", t.code, t.name);
    }
    if titles.len() > 50 {
        println!("  ... {} more", titles.len() - 50);
    }
}

// ---------------------------------------------------------------------------
// triage — audit an exported rip tree without needing the hoot archive
// ---------------------------------------------------------------------------

/// One classified track.
struct TriagedTrack {
    /// Path relative to the triage root, e.g. `pc88/[PC-8801] Foo (OPN)/00 Bar.s98`.
    rel: String,
    class: hoot_log::Audibility,
    markers: hoot_log::Markers,
    /// Identity of the command stream, ignoring the tag block.
    dump_id: (u64, usize),
}

/// FNV-1a 64. Not cryptographic — it only has to group identical byte runs
/// within one set, and it is paired with the region length at every comparison.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn triage(
    dir: &std::path::Path,
    report: Option<&std::path::Path>,
    quarantine: Option<&std::path::Path>,
    apply: bool,
) -> Result<()> {
    use std::io::Write as _;

    if !dir.is_dir() {
        anyhow::bail!("{} is not a directory", dir.display());
    }
    if apply && quarantine.is_none() {
        anyhow::bail!("--apply needs --quarantine <dir>: this command never deletes anything");
    }

    // set directory -> its tracks, in discovery order
    let mut sets: BTreeMap<PathBuf, Vec<TriagedTrack>> = BTreeMap::new();
    let mut unreadable = 0usize;

    for entry in walkdir::WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("s98") {
            continue;
        }
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let parsed = match hoot_log::read_s98(&bytes) {
            Ok(p) => p,
            Err(_) => {
                unreadable += 1;
                continue;
            }
        };
        let region = hoot_log::s98::dump_region(&bytes).unwrap_or(&[]);
        let rel = path.strip_prefix(dir).unwrap_or(path).to_string_lossy().into_owned();
        let set = path.parent().unwrap_or(dir).to_path_buf();
        sets.entry(set).or_default().push(TriagedTrack {
            rel,
            class: hoot_log::audibility_s98(&parsed),
            markers: hoot_log::markers_s98(&parsed),
            dump_id: (fnv1a64(region), region.len()),
        });
    }

    // --- tally -------------------------------------------------------------
    let mut dead = 0usize;
    let mut novoice = 0usize;
    let mut adpcm_only = 0usize;
    let mut audible = 0usize;
    let mut total = 0usize;
    let mut identical_sets = 0usize;
    let mut identical_tracks = 0usize;
    let mut dupgroup_sets = 0usize;

    let mut rep: Option<std::io::BufWriter<std::fs::File>> = match report {
        Some(p) => Some(std::io::BufWriter::new(std::fs::File::create(p)?)),
        None => None,
    };

    let mut to_move: Vec<String> = Vec::new();

    for (set, tracks) in &sets {
        let mut groups: BTreeMap<(u64, usize), usize> = BTreeMap::new();
        for t in tracks {
            *groups.entry(t.dump_id).or_default() += 1;
        }
        let all_identical = tracks.len() > 1 && groups.len() == 1;
        let has_dupes = groups.values().any(|&n| n > 1);
        if all_identical {
            identical_sets += 1;
            identical_tracks += tracks.len();
        } else if has_dupes {
            dupgroup_sets += 1;
        }
        let set_rel = set.strip_prefix(dir).unwrap_or(set).to_string_lossy().into_owned();

        for t in tracks {
            total += 1;
            match t.class {
                hoot_log::Audibility::Dead => dead += 1,
                hoot_log::Audibility::NoVoice => novoice += 1,
                hoot_log::Audibility::AdpcmOnly => adpcm_only += 1,
                hoot_log::Audibility::Audible => audible += 1,
            }
            if t.class.is_silent() {
                to_move.push(t.rel.clone());
            }
            if let Some(w) = rep.as_mut() {
                writeln!(
                    w,
                    "{{\"track\":\"{}\",\"set\":\"{}\",\"class\":\"{}\",\"key_on\":{},\
\"ssg_tone\":{},\"rhythm\":{},\"voiced\":{},\"adpcm\":{},\"dump_hash\":\"{:016x}\",\"dump_len\":{},\
\"set_all_identical\":{},\"set_distinct\":{},\"set_tracks\":{}}}",
                    json_escape(&t.rel),
                    json_escape(&set_rel),
                    t.class.tag(),
                    t.markers.key_on,
                    t.markers.ssg_tone,
                    t.markers.rhythm,
                    t.markers.voiced,
                    t.markers.adpcm,
                    t.dump_id.0,
                    t.dump_id.1,
                    all_identical,
                    groups.len(),
                    tracks.len(),
                )?;
            }
        }
    }
    if let Some(mut w) = rep {
        w.flush()?;
    }

    // --- report ------------------------------------------------------------
    let pct = |n: usize| if total == 0 { 0.0 } else { 100.0 * n as f64 / total as f64 };
    println!("triage {}", dir.display());
    println!("  {total} tracks in {} sets", sets.len());
    if unreadable > 0 {
        println!("  {unreadable} unreadable/unparseable file(s)");
    }
    println!("  audible          {audible:>6}  ({:>5.1}%)", pct(audible));
    println!("  dead (no key-on) {dead:>6}  ({:>5.1}%)", pct(dead));
    println!("  no voice loaded  {novoice:>6}  ({:>5.1}%)", pct(novoice));
    println!(
        "  ADPCM-only       {adpcm_only:>6}  ({:>5.1}%)  [format gap, not a failed rip]",
        pct(adpcm_only)
    );
    println!(
        "  SILENT total     {:>6}  ({:>5.1}%)",
        dead + novoice + adpcm_only,
        pct(dead + novoice + adpcm_only)
    );
    println!(
        "  sets with every track byte-identical: {identical_sets} ({identical_tracks} tracks)"
    );
    println!("  sets with smaller duplicate groups:   {dupgroup_sets}");
    if let Some(p) = report {
        println!("  report written to {}", p.display());
    }

    // --- quarantine --------------------------------------------------------
    let Some(qdir) = quarantine else {
        if !to_move.is_empty() {
            println!(
                "\n  {} silent track(s) would be quarantined; pass --quarantine <dir> --apply to move them",
                to_move.len()
            );
        }
        return Ok(());
    };
    if !apply {
        println!(
            "\n  dry run: {} silent track(s) (plus matching .vgz) would move to {}",
            to_move.len(),
            qdir.display()
        );
        return Ok(());
    }

    let mut moved = 0usize;
    let mut failed = 0usize;
    for rel in &to_move {
        // Move the .s98 and its .vgz twin, preserving <platform>/<set>/ layout.
        for ext in ["s98", "vgz"] {
            let src = dir.join(rel).with_extension(ext);
            if !src.exists() {
                continue;
            }
            let dst = qdir.join(rel).with_extension(ext);
            let Some(parent) = dst.parent() else { continue };
            if std::fs::create_dir_all(parent).is_err() {
                failed += 1;
                continue;
            }
            // rename() fails across filesystems; fall back to copy+remove.
            let ok = std::fs::rename(&src, &dst).is_ok()
                || (std::fs::copy(&src, &dst).is_ok() && std::fs::remove_file(&src).is_ok());
            if ok {
                moved += 1;
            } else {
                failed += 1;
            }
        }
    }
    // A set whose every track was silent leaves an empty folder behind.
    // remove_dir refuses to touch a non-empty directory, so this can only
    // ever clear the ones the move emptied.
    let mut pruned = 0usize;
    for set in sets.keys() {
        if std::fs::remove_dir(set).is_ok() {
            pruned += 1;
        }
    }

    println!("\n  moved {moved} file(s) to {}", qdir.display());
    if pruned > 0 {
        println!("  pruned {pruned} set folder(s) left empty");
    }
    if failed > 0 {
        println!("  {failed} file(s) could not be moved");
    }
    Ok(())
}
