#!/usr/bin/env python3
"""Fixture tests for the live API acceptance helpers."""

import gzip
import tempfile
import unittest
from pathlib import Path

from importlib.util import module_from_spec, spec_from_file_location

SPEC = spec_from_file_location("live_api_acceptance", Path(__file__).with_name("live-api-acceptance.py"))
MODULE = module_from_spec(SPEC)
assert SPEC and SPEC.loader
SPEC.loader.exec_module(MODULE)


class LiveAcceptanceFixtures(unittest.TestCase):
    def test_env_parser_does_not_execute_hostile_values(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".env.live"
            path.write_text("IPTV_TEST_M3U_URL=$(touch /tmp/should-not-exist)\nUNKNOWN=ignored\n", encoding="utf-8")
            self.assertEqual(MODULE.parse_env(path)["IPTV_TEST_M3U_URL"], "$(touch /tmp/should-not-exist)")

    def test_gzip_xmltv_stream_and_programme_sample(self) -> None:
        payload = b'<tv><channel id="abc"/><programme channel="abc" start="20260101000000 +0000" stop="20260101010000 +0000"><title>News</title></programme></tv>'
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "guide.xmltv.gz"
            path.write_bytes(gzip.compress(payload))
            expanded = Path(directory) / "guide.xmltv"
            with gzip.open(path, "rb") as source, expanded.open("wb") as target:
                target.write(source.read())
            ids, programmes, _ = MODULE.xmltv_file(expanded)
            self.assertEqual(ids, {"abc"})
            self.assertEqual(programmes["abc"][0][0], "News")

    def test_page_and_canonical_key_helpers(self) -> None:
        self.assertEqual(MODULE.page_items({"items": [{"canonicalKey": "abc"}]}), [{"canonicalKey": "abc"}])
        self.assertEqual(MODULE.channel_key({"canonicalKey": "abc", "tvgId": "legacy"}), "abc")


if __name__ == "__main__":
    unittest.main()
