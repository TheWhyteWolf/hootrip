//! Event-driven parser for hoot gamelist XML.
//!
//! The source files are 2005-2018-era Shift_JIS XML with CRLF line endings and
//! occasional rough edges, so parsing is deliberately lenient: unknown elements
//! are skipped, malformed entities fall back to raw text, and per-file failures
//! are reported rather than aborting a whole catalogue load.

use std::path::Path;

use anyhow::{Context, Result};
use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use crate::model::*;

/// Decode a hoot XML file honouring its declared encoding (Shift_JIS or UTF-8).
pub fn decode_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(decode_bytes(&bytes))
}

/// Decode raw bytes: sniff the XML declaration, default to Shift_JIS (the
/// archive's dominant encoding) when no declaration is found.
pub fn decode_bytes(bytes: &[u8]) -> String {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).to_lowercase();
    let shift_jis = match head.find("encoding=") {
        Some(_) => head.contains("shift_jis") || head.contains("shift-jis") || head.contains("sjis"),
        None => true,
    };
    if shift_jis {
        let (s, _, _) = encoding_rs::SHIFT_JIS.decode(bytes);
        s.into_owned()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

pub fn parse_gamelist_file(path: &Path) -> Result<GameList> {
    let text = decode_file(path)?;
    parse_gamelist_str(&text).with_context(|| format!("parsing {}", path.display()))
}

pub fn parse_gamelist_str(text: &str) -> Result<GameList> {
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = false;

    let mut list = GameList::default();
    let mut game: Option<Game> = None;
    let mut bind: Option<Bind> = None;
    let mut romlist: Option<RomList> = None;
    let mut in_titlelist = false;
    let mut in_options = false;
    let mut in_exts = false;

    // Attributes of the text-bearing element currently open, plus its text.
    let mut pend_attrs: Vec<(String, String)> = Vec::new();
    let mut text_buf = String::new();
    let mut capture = false;

    loop {
        match reader.read_event() {
            Err(e) => return Err(e).context("XML syntax error"),
            Ok(Event::Eof) => break,
            Ok(Event::Start(e)) => {
                let name = local_name(&e);
                match name.as_str() {
                    "gamelist" => list.date = get_attr(&e, "date"),
                    "game" => game = Some(Game::default()),
                    "bind" => bind = Some(Bind::default()),
                    "romlist" => {
                        romlist = Some(RomList {
                            archive: get_attr(&e, "archive"),
                            roms: Vec::new(),
                        })
                    }
                    "titlelist" => in_titlelist = true,
                    "options" => in_options = true,
                    "exts" => in_exts = true,
                    "childlists" => {}
                    // Text-bearing leaves: start capturing.
                    "name" | "driver" | "driveralias" | "rom" | "title" | "range" | "list"
                    | "ext" => {
                        pend_attrs = all_attrs(&e);
                        text_buf.clear();
                        capture = true;
                    }
                    "option" => {
                        // <option> is EMPTY per DTD but tolerate the Start form.
                        push_option(&e, &mut game, &mut bind, in_options);
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let name = local_name(&e);
                match name.as_str() {
                    "option" => push_option(&e, &mut game, &mut bind, in_options),
                    // Tolerate self-closed leaves (empty text).
                    "name" | "driver" | "driveralias" | "rom" | "title" | "range" | "list"
                    | "ext" => {
                        pend_attrs = all_attrs(&e);
                        finish_leaf(
                            &name,
                            "",
                            &pend_attrs,
                            &mut list,
                            &mut game,
                            &mut bind,
                            &mut romlist,
                            in_titlelist,
                            in_exts,
                        );
                    }
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                if capture {
                    match e.unescape() {
                        Ok(s) => text_buf.push_str(&s),
                        Err(_) => text_buf.push_str(&String::from_utf8_lossy(e.as_ref())),
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = local_name_end(e.name().as_ref());
                match name.as_str() {
                    "game" => {
                        if let Some(g) = game.take() {
                            list.games.push(g);
                        }
                    }
                    "bind" => {
                        if let Some(b) = bind.take() {
                            list.binds.push(b);
                        }
                    }
                    "romlist" => {
                        if let (Some(g), Some(r)) = (game.as_mut(), romlist.take()) {
                            g.romlist = Some(r);
                        }
                    }
                    "titlelist" => in_titlelist = false,
                    "options" => in_options = false,
                    "exts" => in_exts = false,
                    "name" | "driver" | "driveralias" | "rom" | "title" | "range" | "list"
                    | "ext" => {
                        if capture {
                            let text = text_buf.trim_matches(['\r', '\n', '\t']).trim();
                            finish_leaf(
                                &name,
                                text,
                                &pend_attrs,
                                &mut list,
                                &mut game,
                                &mut bind,
                                &mut romlist,
                                in_titlelist,
                                in_exts,
                            );
                            capture = false;
                            text_buf.clear();
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    Ok(list)
}

#[allow(clippy::too_many_arguments)]
fn finish_leaf(
    name: &str,
    text: &str,
    attrs: &[(String, String)],
    list: &mut GameList,
    game: &mut Option<Game>,
    bind: &mut Option<Bind>,
    romlist: &mut Option<RomList>,
    in_titlelist: bool,
    in_exts: bool,
) {
    let attr = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    match name {
        "name" => {
            if let Some(g) = game.as_mut() {
                g.name = text.to_string();
            }
        }
        "driver" => {
            let d = Driver {
                platform: text.to_string(),
                kind: attr("type"),
            };
            if let Some(g) = game.as_mut() {
                g.driver = d;
            } else if let Some(b) = bind.as_mut() {
                b.driver = d;
            }
        }
        "driveralias" => {
            if let Some(g) = game.as_mut() {
                g.driver_alias = Some(DriverAlias {
                    label: text.to_string(),
                    kind: attr("type"),
                });
            }
        }
        "rom" => {
            if let Some(r) = romlist.as_mut() {
                r.roms.push(Rom {
                    kind: attr("type").unwrap_or_default(),
                    offset: attr("offset").and_then(|v| parse_num(&v)),
                    crc32: attr("crc32").and_then(|v| {
                        let v = v.trim();
                        let v = v.strip_prefix("0x").unwrap_or(v);
                        u32::from_str_radix(v, 16).ok()
                    }),
                    name: text.to_string(),
                });
            }
        }
        "title" if in_titlelist => {
            if let (Some(g), Some(code)) = (game.as_mut(), attr("code").and_then(|c| parse_unum(&c)))
            {
                g.titles.push(TitleEntry::Title(Title {
                    code,
                    name: text.to_string(),
                    kind: attr("type"),
                }));
            }
        }
        "range" if in_titlelist => {
            if let Some(g) = game.as_mut() {
                let (min, max) = (
                    attr("min").and_then(|v| parse_unum(&v)),
                    attr("max").and_then(|v| parse_unum(&v)),
                );
                if let (Some(min), Some(max)) = (min, max) {
                    g.titles.push(TitleEntry::Range(TitleRange {
                        min,
                        max,
                        extcode: attr("extcode").and_then(|v| parse_unum(&v)),
                        format: text.to_string(),
                    }));
                }
            }
        }
        "list" => list.childlists.push(text.replace('\\', "/")),
        "ext" if in_exts => {
            if let Some(b) = bind.as_mut() {
                b.exts.push(text.to_lowercase());
            }
        }
        _ => {}
    }
}

fn push_option(e: &BytesStart, game: &mut Option<Game>, bind: &mut Option<Bind>, in_options: bool) {
    if !in_options {
        return;
    }
    let (name, value) = (get_attr(e, "name"), get_attr(e, "value"));
    if let (Some(name), Some(value)) = (name, value) {
        let opt = GameOption { name, value };
        if let Some(g) = game.as_mut() {
            g.options.push(opt);
        } else if let Some(b) = bind.as_mut() {
            b.options.push(opt);
        }
    }
}

fn local_name(e: &BytesStart) -> String {
    String::from_utf8_lossy(e.name().as_ref()).to_lowercase()
}

fn local_name_end(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).to_lowercase()
}

fn get_attr(e: &BytesStart, want: &str) -> Option<String> {
    for a in e.attributes().with_checks(false).flatten() {
        if String::from_utf8_lossy(a.key.as_ref()).eq_ignore_ascii_case(want) {
            return Some(match a.unescape_value() {
                Ok(v) => v.into_owned(),
                Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
            });
        }
    }
    None
}

fn all_attrs(e: &BytesStart) -> Vec<(String, String)> {
    e.attributes()
        .with_checks(false)
        .flatten()
        .map(|a| {
            (
                String::from_utf8_lossy(a.key.as_ref()).to_lowercase(),
                match a.unescape_value() {
                    Ok(v) => v.into_owned(),
                    Err(_) => String::from_utf8_lossy(&a.value).into_owned(),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="Shift_JIS"?>
<!DOCTYPE gamelist [
	<!ELEMENT gamelist (game|bind|childlists)*>
]>
<gamelist date="2006/06/19">
	<game>
		<name>[PC-8801] The 4th Unit (OPN)</name>
		<driver type="opn">pc88</driver>
		<driveralias type="NEC PC-8801">Data West</driveralias>
		<options>
			<option name="mdata_addr" value="0x7000"/>
			<option name="use_rtc" value="0x01"/>
		</options>
		<romlist archive="4thunit">
			<rom type="code" offset="0x0000">PATCH</rom>
			<rom type="code" offset="0x1000">PROG</rom>
			<rom type="bgm" offset="0x00">MUS00</rom>
			<rom type="file" offset="-1">MUSIC.COM</rom>
		</romlist>
		<titlelist>
			<title code="0x01000000">MUS00</title>
			<range min="0x00" max="0x03" extcode="1">MUSIC.DAT : 0x%02x</range>
		</titlelist>
	</game>
	<bind>
		<exts><ext>MDX</ext></exts>
		<driver type="mxdrv">x68k</driver>
		<options><option name="opm_mix" value="0xc0"/></options>
	</bind>
	<childlists>
		<list>xml2\new_sets.xml</list>
		<list>xml/datawest.xml</list>
	</childlists>
</gamelist>"#;

    #[test]
    fn parses_full_sample() {
        let l = parse_gamelist_str(SAMPLE).unwrap();
        assert_eq!(l.date.as_deref(), Some("2006/06/19"));
        assert_eq!(l.games.len(), 1);
        let g = &l.games[0];
        assert_eq!(g.name, "[PC-8801] The 4th Unit (OPN)");
        assert_eq!(g.driver.platform, "pc88");
        assert_eq!(g.driver.kind.as_deref(), Some("opn"));
        assert_eq!(g.driver_alias.as_ref().unwrap().label, "Data West");
        assert_eq!(g.options.len(), 2);
        assert_eq!(g.options[0].name, "mdata_addr");
        let rl = g.romlist.as_ref().unwrap();
        assert_eq!(rl.archive.as_deref(), Some("4thunit"));
        assert_eq!(rl.roms.len(), 4);
        assert_eq!(rl.roms[1].offset, Some(0x1000));
        assert_eq!(rl.roms[3].offset, Some(-1));
        let titles = g.expanded_titles();
        assert_eq!(titles.len(), 5);
        assert_eq!(titles[0].code, 0x0100_0000);
        assert_eq!(titles[4].name, "MUSIC.DAT : 0x03");
        assert_eq!(l.binds.len(), 1);
        assert_eq!(l.binds[0].exts, vec!["mdx"]);
        assert_eq!(l.binds[0].driver.platform, "x68k");
        assert_eq!(
            l.childlists,
            vec!["xml2/new_sets.xml".to_string(), "xml/datawest.xml".into()]
        );
    }

    #[test]
    fn shift_jis_decode() {
        // "ソ" (0x83\x5C) contains a backslash byte — the classic Shift_JIS trap.
        let bytes = b"<?xml version=\"1.0\" encoding=\"Shift_JIS\"?>\r\n<gamelist><game><name>\x83\x5C\x83\x8B</name><driver>pc88</driver></game></gamelist>";
        let text = decode_bytes(bytes);
        let l = parse_gamelist_str(&text).unwrap();
        assert_eq!(l.games[0].name, "ソル");
    }
}
