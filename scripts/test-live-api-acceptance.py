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
            path.write_text("URL=$(touch /tmp/should-not-exist)\nUNKNOWN=ignored\n", encoding="utf-8")
            self.assertEqual(MODULE.parse_env(path)["URL"], "$(touch /tmp/should-not-exist)")

    def test_xtream_env_and_endpoint_are_credential_aware(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / ".env.xtreme"
            path.write_text("URL=https://provider.example:8080/panel\nUSER=alice\nPWD=secret\n", encoding="utf-8")
            values = MODULE.parse_env(path)
        self.assertEqual(values["USER"], "alice")
        endpoint = MODULE.xtream_endpoint(values["URL"], values["USER"], values["PWD"], "player_api.php", "get_live_streams")
        self.assertIn("action=get_live_streams", endpoint)
        self.assertIn("username=alice", endpoint)
        self.assertNotIn("provider-password", endpoint)

    def test_provider_ids_are_taken_from_epg_channel_id(self) -> None:
        streams = [{"stream_id": 7, "epg_channel_id": "news.example"}, {"stream_id": 8, "epg_channel_id": ""}]
        ids = {str(item.get("epg_channel_id", "")).strip() for item in streams if str(item.get("epg_channel_id", "")).strip()}
        self.assertEqual(ids, {"news.example"})

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
