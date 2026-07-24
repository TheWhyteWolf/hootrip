//! Data model for hoot gamelist XML descriptors.
//!
//! Schema source: the DTD embedded in every gamelist file, e.g.
//! `xml/datawest.xml` in HootArchive20180626. There is no external spec.

/// One parsed gamelist file (`hoot.xml`, `xml/*.xml`, `xml2/*.xml`).
#[derive(Debug, Default, Clone)]
pub struct GameList {
    /// `<gamelist date="...">`
    pub date: Option<String>,
    pub games: Vec<Game>,
    /// Extension-based auto-detection rules (only present in `hoot.xml`).
    pub binds: Vec<Bind>,
    /// `<childlists><list>` include paths, relative to the archive root.
    pub childlists: Vec<String>,
}

/// `<bind>`: maps file extensions to a driver for sets with no `<game>` entry.
#[derive(Debug, Default, Clone)]
pub struct Bind {
    pub exts: Vec<String>,
    pub driver: Driver,
    pub options: Vec<GameOption>,
}

#[derive(Debug, Default, Clone)]
pub struct Game {
    pub name: String,
    pub driver: Driver,
    pub driver_alias: Option<DriverAlias>,
    pub options: Vec<GameOption>,
    pub romlist: Option<RomList>,
    pub titles: Vec<TitleEntry>,
}

/// `<driver type="opn">pc88</driver>`:
/// text = emulated machine/board, `type` = sound-driver kind.
#[derive(Debug, Default, Clone)]
pub struct Driver {
    pub platform: String,
    pub kind: Option<String>,
}

/// `<driveralias type="NEC PC-8801">Data West</driveralias>` — display only.
#[derive(Debug, Default, Clone)]
pub struct DriverAlias {
    pub label: String,
    pub kind: Option<String>,
}

/// `<option name="mdata_addr" value="0x7000"/>` — driver config pokes.
#[derive(Debug, Clone)]
pub struct GameOption {
    pub name: String,
    /// Raw string; use [`crate::parse_num`] when a numeric value is expected.
    pub value: String,
}

/// `<romlist archive="4thunit">` — `archive` names the set folder holding the files.
#[derive(Debug, Default, Clone)]
pub struct RomList {
    pub archive: Option<String>,
    pub roms: Vec<Rom>,
}

#[derive(Debug, Clone)]
pub struct Rom {
    /// `type=`: observed values `code`, `bgm`, `file`, `shell`, plus rarities.
    pub kind: String,
    /// Load address / file slot / song bank, driver-dependent. `-1` = "as executable".
    pub offset: Option<i64>,
    pub crc32: Option<u32>,
    /// File name within the archive folder, or the shell command for `type="shell"`.
    pub name: String,
}

/// `<titlelist>` entry: either a literal `<title>` or a `<range>` batch.
#[derive(Debug, Clone)]
pub enum TitleEntry {
    Title(Title),
    Range(TitleRange),
}

/// `<title code="0x01000002">MUS02</title>` — `code` is the song-select value
/// poked into the driver; semantics of the packing are driver-kind-specific.
#[derive(Debug, Clone)]
pub struct Title {
    pub code: u64,
    pub name: String,
    pub kind: Option<String>,
}

/// `<range min="0x00" max="0x03" extcode="1">MUSIC.DAT : 0x%02x</range>`
/// generates one title per code in `min..=max`, naming it with the printf-style
/// format in the text. `extcode` semantics are not yet understood (TODO).
#[derive(Debug, Clone)]
pub struct TitleRange {
    pub min: u64,
    pub max: u64,
    pub extcode: Option<u64>,
    pub format: String,
}

impl TitleRange {
    /// Expand to concrete titles. The format receives the code value.
    pub fn expand(&self) -> Vec<Title> {
        (self.min..=self.max)
            .map(|code| Title {
                code,
                name: format_code(&self.format, code),
                kind: None,
            })
            .collect()
    }
}

impl Game {
    /// All songs, with `<range>` entries expanded.
    pub fn expanded_titles(&self) -> Vec<Title> {
        let mut out = Vec::new();
        for t in &self.titles {
            match t {
                TitleEntry::Title(t) => out.push(t.clone()),
                TitleEntry::Range(r) => out.extend(r.expand()),
            }
        }
        out
    }
}

/// Minimal printf for the conversions seen in hoot titlelists:
/// `%X %x %d` with optional zero-pad/width (e.g. `%02x`, `%2X`).
pub fn format_code(fmt: &str, code: u64) -> String {
    let mut out = String::new();
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut zero_pad = false;
        let mut width = 0usize;
        let conv = loop {
            match chars.next() {
                Some('0') if width == 0 && !zero_pad => zero_pad = true,
                Some(d @ '0'..='9') => width = width * 10 + (d as usize - '0' as usize),
                Some(c @ ('x' | 'X' | 'd' | 'u')) => break Some(c),
                Some('%') => break None,
                other => {
                    // Unknown conversion: emit literally and stop parsing this spec.
                    out.push('%');
                    if let Some(o) = other {
                        out.push(o);
                    }
                    break None;
                }
            }
        };
        match conv {
            Some('x') => push_padded(&mut out, &format!("{code:x}"), width, zero_pad),
            Some('X') => push_padded(&mut out, &format!("{code:X}"), width, zero_pad),
            Some('d' | 'u') => push_padded(&mut out, &format!("{code}"), width, zero_pad),
            Some(_) => unreachable!(),
            None => {
                if conv.is_none() && width == 0 && !zero_pad {
                    // was `%%` or bailed above
                }
            }
        }
    }
    out
}

fn push_padded(out: &mut String, s: &str, width: usize, zero_pad: bool) {
    let pad = width.saturating_sub(s.len());
    for _ in 0..pad {
        out.push(if zero_pad { '0' } else { ' ' });
    }
    out.push_str(s);
}

/// Parse hoot numeric literals: `0x7000`, `123`, `-1`.
pub fn parse_num(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else if let Some(hex) = s.strip_prefix("-0x").or_else(|| s.strip_prefix("-0X")) {
        i64::from_str_radix(hex, 16).ok().map(|v| -v)
    } else {
        s.parse().ok()
    }
}

/// Parse an unsigned hoot literal (title codes, range bounds).
pub fn parse_unum(s: &str) -> Option<u64> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        s.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printf_variants() {
        assert_eq!(format_code("MUSIC.DAT : 0x%02x", 0x3), "MUSIC.DAT : 0x03");
        assert_eq!(format_code("YUME.EFC  : %2X", 0x80d), "YUME.EFC  : 80D");
        assert_eq!(format_code("track %d", 12), "track 12");
        assert_eq!(format_code("no spec", 1), "no spec");
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_num("0x7000"), Some(0x7000));
        assert_eq!(parse_num("-1"), Some(-1));
        assert_eq!(parse_num(" 8 "), Some(8));
        assert_eq!(parse_unum("0x01000002"), Some(0x0100_0002));
    }

    #[test]
    fn range_expansion() {
        let r = TitleRange {
            min: 0,
            max: 2,
            extcode: None,
            format: "M%02d".into(),
        };
        let t = r.expand();
        assert_eq!(t.len(), 3);
        assert_eq!(t[2].name, "M02");
        assert_eq!(t[2].code, 2);
    }
}
