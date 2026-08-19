#![no_main]

use std::io::Cursor;

use iptv_parsers::{
    ParseLimits, parse_xtream_auth, parse_xtream_live_categories, parse_xtream_live_streams,
    parse_xtream_short_epg,
};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let limits = fuzz_limits();
    let _ = parse_xtream_auth(Cursor::new(data), limits);
    let _ = parse_xtream_live_categories(Cursor::new(data), limits);
    let _ = parse_xtream_live_streams(Cursor::new(data), limits);
    let _ = parse_xtream_short_epg(Cursor::new(data), limits);
});

fn fuzz_limits() -> ParseLimits {
    ParseLimits {
        max_input_bytes: 1024 * 1024,
        max_line_bytes: 64 * 1024,
        max_records: 4_096,
        max_text_bytes: 64 * 1024,
        max_xml_depth: 32,
    }
}
