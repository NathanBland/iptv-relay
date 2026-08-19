#![no_main]

use std::io::Cursor;

use iptv_parsers::{CompiledEventRules, ParseLimits, parse_event_rules};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(document) = parse_event_rules(Cursor::new(data), fuzz_limits()) {
        let _ = CompiledEventRules::new(document.rules);
    }
});

fn fuzz_limits() -> ParseLimits {
    ParseLimits {
        max_input_bytes: 1024 * 1024,
        max_line_bytes: 64 * 1024,
        max_records: 1_024,
        max_text_bytes: 64 * 1024,
        max_xml_depth: 32,
    }
}
