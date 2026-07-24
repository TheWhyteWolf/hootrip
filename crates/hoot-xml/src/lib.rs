//! hoot-xml: parser for the hoot emulator's gamelist XML descriptors
//! (HootArchive-style layouts) and archive catalogue loading.

pub mod catalogue;
pub mod model;
pub mod parse;

pub use catalogue::{Catalogue, DEFAULT_DATA_DIRS};
pub use model::{
    format_code, parse_num, parse_unum, Bind, Driver, DriverAlias, Game, GameList, GameOption,
    Rom, RomList, Title, TitleEntry, TitleRange,
};
pub use parse::{decode_bytes, decode_file, parse_gamelist_file, parse_gamelist_str};
