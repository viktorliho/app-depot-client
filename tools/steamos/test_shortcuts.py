from dataclasses import asdict
import io
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest

import vdf
from PIL import Image
from shortcuts import Binding, artwork_name, fixture_target, muted_grid, reconcile, shortcut_id


PROFILE = "10000000-0000-0000-0000-000000000000"
APP = "20000000-0000-0000-0000-000000000000"
OTHER = "30000000-0000-0000-0000-000000000000"
FOREIGN = {"appid": -2, "AppName": "Heroic", "LaunchOptions": "user-options"}
EMPTY = vdf.binary_dumps({"shortcuts": {"0": FOREIGN}})


class ShortcutTests(unittest.TestCase):
    def test_all_apps_idempotent_and_rename_preserves_identity(self):
        first, ledger = reconcile(EMPTY, PROFILE, {APP: "Native", OTHER: "Data"}, {})
        self.assertEqual(reconcile(first, PROFILE, {APP: "Native", OTHER: "Data"}, ledger),
                         (first, ledger))
        wire_ledger = json.dumps({app: asdict(binding) for app, binding in ledger.items()})
        reloaded_ledger = {app: Binding(**fields) for app, fields in json.loads(wire_ledger).items()}
        decoded = vdf.binary_loads(first)
        decoded["shortcuts"]["1"]["LaunchOptions"] = "user-override"
        decoded["shortcuts"]["1"]["Exe"] = '"/fixture/windows-game.exe"'
        changed = vdf.binary_dumps(decoded)
        renamed, next_ledger = reconcile(changed, PROFILE, {APP: "Renamed"}, reloaded_ledger)
        rows = vdf.binary_loads(renamed)["shortcuts"]
        self.assertEqual(rows["0"], FOREIGN)
        self.assertEqual(rows["1"]["AppName"], "Renamed")
        self.assertEqual(rows["1"]["LaunchOptions"], "user-override")
        self.assertEqual(rows["1"]["Exe"], '"/fixture/windows-game.exe"')
        self.assertEqual(rows["2"]["AppName"], "Data")
        self.assertEqual(ledger[APP].appid, next_ledger[APP].appid)
        self.assertEqual(rows["1"]["appid"] & 0xFFFFFFFF, ledger[APP].appid)

    def test_collision_and_missing_binding_fail_closed(self):
        first, saved = reconcile(EMPTY, PROFILE, {APP: "Native"}, {})
        missing_marker = vdf.binary_loads(first)
        missing_marker["shortcuts"]["1"]["tags"] = {}
        with self.assertRaises(ValueError):
            reconcile(vdf.binary_dumps(missing_marker), PROFILE, {APP: "Native"}, saved)
        with self.assertRaises(ValueError):
            reconcile(first, PROFILE, {APP: "Native"}, {})
        collision = shortcut_id(fixture_target(PROFILE, OTHER), "Native")
        foreign = dict(FOREIGN, appid=collision - (1 << 32))
        payload = vdf.binary_dumps({"shortcuts": {"0": foreign}})
        with self.assertRaises(ValueError):
            reconcile(payload, PROFILE, {OTHER: "Native"}, {})
        _, ledger = reconcile(EMPTY, PROFILE, {APP: "Native", OTHER: "Native"}, {})
        self.assertNotEqual(ledger[APP].appid, ledger[OTHER].appid)
        with self.assertRaises(SyntaxError):
            reconcile(first[:-5], PROFILE, {APP: "Native"}, {})

    def test_reference_crc_changes_with_name_and_target(self):
        exe = '"/tmp/app-depot-wrapper"'
        self.assertEqual(shortcut_id(exe, "Synthetic App"), 0x82B468B5)
        self.assertNotEqual(shortcut_id(exe, "Rename"), 0x82B468B5)
        self.assertNotEqual(shortcut_id('"/tmp/game.exe"', "Synthetic App"), 0x82B468B5)

    def test_artwork_output_and_original(self):
        appid = 0x82B468B5
        self.assertEqual([artwork_name(appid, role) for role in
                          ("vertical", "horizontal", "hero", "logo")],
                         ["2192861365p.png", "2192861365.png",
                          "2192861365_hero.png", "2192861365_logo.png"])
        source = Image.new("RGBA", (2, 1), (255, 40, 15, 125))
        encode = self.png_bytes
        original = encode(source)
        muted = muted_grid(source)
        r, g, b, alpha = muted.getpixel((0, 0))
        self.assertEqual((r, g, b, alpha), (r, r, r, 125))
        self.assertEqual(muted.size, source.size)
        self.assertEqual(encode(muted), encode(muted_grid(source)))
        self.assertEqual(encode(source), original)
        with TemporaryDirectory() as fixture:
            grid = Path(fixture) / "userdata" / "123" / "config" / "grid"
            grid.mkdir(parents=True)
            for role in ("vertical", "horizontal"):
                target = grid / artwork_name(appid, role)
                target.write_bytes(encode(muted))
                self.assertEqual(Image.open(target).getpixel((0, 0)), muted.getpixel((0, 0)))
                target.write_bytes(original)
                self.assertEqual(Image.open(target).getpixel((0, 0)), source.getpixel((0, 0)))

    @staticmethod
    def png_bytes(image):
        out = io.BytesIO()
        image.save(out, format="PNG")
        return out.getvalue()


if __name__ == "__main__":
    unittest.main()
