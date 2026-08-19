#![no_main]

use std::io::Cursor;

use iptv_parsers::{ParseLimits, parse_xmltv};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = parse_xmltv(Cursor::new(data), fuzz_limits());
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
