//! Generate a small audible YM2203 test piece as .vgm/.vgz/.s98 for
//! validation against independent players (libvgm's vgm2wav, S98 players).
//!
//! Usage: cargo run -p hoot-log --example gen_test -- <outdir>

use hoot_log::s98::{write_s98, S98Tags};
use hoot_log::{write_vgm, write_vgz, Chip, Device, Gd3, RegisterLog, RegWrite};

fn main() -> anyhow::Result<()> {
    let outdir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    std::fs::create_dir_all(&outdir)?;

    // 1 MHz tick clock; YM2203 at the PC-88 clock.
    let mut log = RegisterLog::new(
        1_000_000,
        vec![Device {
            chip: Chip::Ym2203,
            clock_hz: 3_993_600,
        }],
    );

    let mut t = 0u64;
    let w = |log: &mut RegisterLog, t: u64, addr: u8, data: u8| {
        log.push(RegWrite { t, dev: 0, port: 0, addr, data });
    };

    // SSG square-wave melody on channel A (registers 0x00-0x0D of the OPN).
    w(&mut log, t, 0x07, 0x3E); // mixer: tone A on, B/C and noise off
    w(&mut log, t, 0x08, 0x0F); // channel A volume max

    // A little arpeggio: tone periods for roughly A-C#-E-A over 2 seconds.
    let periods: [u16; 8] = [284, 225, 189, 142, 189, 225, 284, 142];
    for p in periods {
        w(&mut log, t, 0x00, (p & 0xFF) as u8);
        w(&mut log, t, 0x01, (p >> 8) as u8);
        t += 250_000; // 250 ms per step
    }
    w(&mut log, t, 0x08, 0x00); // silence
    log.end_t = t + 100_000;
    log.loop_t = Some(0);

    let mut gd3 = Gd3::default();
    gd3.track_en = "hootrip writer validation".into();
    gd3.game_en = "hootrip test suite".into();
    gd3.system_en = "NEC PC-8801".into();
    gd3.ripper = "hootrip".into();

    let mut tags = S98Tags::default();
    tags.set("title", "hootrip writer validation");
    tags.set("system", "NEC PC-8801");
    tags.set("s98by", "hootrip");

    std::fs::write(format!("{outdir}/test.vgm"), write_vgm(&log, &gd3)?)?;
    std::fs::write(format!("{outdir}/test.vgz"), write_vgz(&log, &gd3)?)?;
    std::fs::write(format!("{outdir}/test.s98"), write_s98(&log, &tags)?)?;
    println!("wrote test.vgm / test.vgz / test.s98 to {outdir}");
    Ok(())
}
