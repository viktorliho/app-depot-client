import asyncio
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import io
import json
from pathlib import Path
import stat
import tempfile
import threading
import unittest
from urllib.error import HTTPError
from urllib.request import Request
from zipfile import ZipFile, ZipInfo

import vdf

import installer
import main
import package


PROFILE = "10000000-0000-0000-0000-000000000000"
OTHER = "10000000-0000-0000-0000-000000000001"
APP = "20000000-0000-0000-0000-000000000000"
ACCOUNT = "111"
ID = 0x80000004


class ConnectionTests(unittest.IsolatedAsyncioTestCase):
    async def test_rpc_reports_expected_errors_and_hides_exception_details(self):
        plugin = main.Plugin()

        async def rejected(*_):
            raise main.RpcFailure("anonymous_reading_disabled")

        plugin.test = rejected
        self.assertEqual(await plugin.request("test", "http://test.local", "", True),
                         {"ok": False, "code": "anonymous_reading_disabled"})

        async def broken():
            raise RuntimeError("fixture secret and private path")

        plugin.discover = broken
        with self.assertLogs("main", level="WARNING") as logs:
            self.assertEqual(await plugin.request("discover"), {"ok": False, "code": "internal"})
        self.assertNotIn("fixture secret", str(logs.output))
        self.assertEqual(await plugin.request("_rust"), {"ok": False, "code": "internal"})


class ShortcutTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.plugin = main.Plugin()
        self.plugin.home = self.root
        self.plugin.plugin = self.root / "plugin"
        self.plugin.data = self.root / "private"
        self.plugin.data.mkdir()
        self.plugin.lock = asyncio.Lock()
        self.plugin.ledger = self.plugin.data / "bindings.json"
        self.config = self.plugin._config(ACCOUNT)
        self.config.mkdir(parents=True)
        (self.config / "grid").mkdir()
        self.path = self.config / "shortcuts.vdf"
        self.foreign = {"appid": -3, "AppName": "Foreign", "Exe": "foreign", "LaunchOptions": "user settings"}
        self.write(self.foreign)

    def write(self, *rows):
        self.path.write_bytes(vdf.binary_dumps({"shortcuts": dict(zip(map(str, range(len(rows))), rows))}))

    def owned(self, plan, name="Same", ident=ID):
        return {"appid": ident - (1 << 32), "AppName": name,
                "Exe": f'"{plan["target"]}"', "LaunchOptions": plan["options"]}

    async def test_pending_readback_restart_profiles_and_renames(self):
        before = self.path.read_bytes()
        plan = await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same")
        self.assertEqual(plan["action"], "create")
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual((await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same"))["action"], "pending")
        await self.plugin.acknowledge(ACCOUNT, PROFILE, APP, ID)
        self.assertEqual(await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same"),
                         {"action": "inspect", "appid": ID})
        live = dict(appid=ID, appname="Same", exe=f'"{plan["target"]}"',
                    startdir=f'"{Path(plan["target"]).parent}"', launchoptions=plan["options"])
        self.assertEqual(await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same", live),
                         {"action": "keep", "appid": ID})
        self.assertEqual((await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Rename", live))["action"], "rename")
        live["appname"] = "Rename"
        self.assertEqual((await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Rename", live))["action"], "keep")
        live["appname"] = "User override"
        self.assertEqual((await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Other", live))["action"], "keep")
        other = await self.plugin.shortcut(ACCOUNT, OTHER, APP, "Same")
        self.assertEqual(other["action"], "create")
        self.assertNotEqual(other["target"], plan["target"])

    async def test_repair_beta2_disk_fingerprint_and_resume_without_recreating(self):
        plan = await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Fixture App")
        await self.plugin.acknowledge(ACCOUNT, PROFILE, APP, ID)
        ledger = self.plugin._load()
        binding = next(iter(ledger.values()))
        del binding["initializing"]
        self.plugin._save(ledger)
        target = plan["target"]
        cwd = str(Path(target).parent)
        damaged = {"appid": ID, "appname": 'play"', "exe": f'""{target}""',
                   "startdir": f'"{cwd}/', "launchoptions": ""}
        with self.assertRaisesRegex(RuntimeError, "ownership"):
            await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Fixture App", damaged)
        disk = dict(damaged, appid=ID - (1 << 32))
        self.write(self.foreign, disk)
        before = self.path.read_bytes()
        planned = await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Fixture App", damaged)
        self.assertEqual(planned["action"], "setup")
        self.assertEqual(planned["appid"], ID)
        self.assertEqual(planned["fields"], {"appname": "Fixture App", "exe": f'"{target}"',
                                             "startdir": f'"{cwd}"', "launchoptions": plan["options"]})
        partial = {**damaged, "exe": f'"{target}"'}
        resumed = await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Fixture App", partial)
        self.assertNotIn("exe", resumed["fields"])
        with self.assertRaisesRegex(RuntimeError, "changed during repair"):
            await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Fixture App",
                                       {**partial, "appname": "My custom name"})
        completed = {**damaged, **planned["fields"]}
        self.assertEqual(await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Fixture App", completed),
                         {"action": "keep", "appid": ID})
        self.assertEqual(self.path.read_bytes(), before)
        self.assertEqual(await self.plugin.binding(ACCOUNT, PROFILE, APP), ID)

    async def test_collision_and_missing_marker(self):
        plan = await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same")
        await self.plugin.acknowledge(ACCOUNT, PROFILE, APP, ID)
        foreign = {key.lower(): value for key, value in {**self.foreign, "appid": ID}.items()}
        with self.assertRaisesRegex(RuntimeError, "ownership"):
            await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same", foreign)
        owned = {key.lower(): value for key, value in self.owned(plan).items()}
        owned["appid"] = ID + 1
        with self.assertRaisesRegex(RuntimeError, "ownership"):
            await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same", owned)
        with self.assertRaises(ValueError):
            await self.plugin.shortcut("../etc", PROFILE, APP, "Same")

    async def test_artwork_readback_idempotence_preserves_override(self):
        plan = await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same")
        self.write(self.foreign, self.owned(plan))
        await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same")

        async def rust(*args):
            import base64
            color, known = args[-2:]
            next_revision = f"version-1:{color}"
            result = {"revision": next_revision}
            if known != next_revision:
                result["png"] = base64.b64encode(color.encode()).decode()
            return result

        self.plugin._rust = rust
        role = "steam.grid.vertical"
        path = self.config / "grid" / f"{ID}p.png"
        self.assertIsNotNone(await self.plugin.artwork(ACCOUNT, PROFILE, APP, role, False))
        path.write_bytes(b"muted")
        self.assertIsNone(await self.plugin.artwork(ACCOUNT, PROFILE, APP, role, False))
        self.assertIsNotNone(await self.plugin.artwork(ACCOUNT, PROFILE, APP, role, True))
        path.write_bytes(b"color")
        self.assertIsNone(await self.plugin.artwork(ACCOUNT, PROFILE, APP, role, True))
        path.write_bytes(b"user custom image")
        self.assertIsNone(await self.plugin.artwork(ACCOUNT, PROFILE, APP, role, False))
        self.assertEqual(path.read_bytes(), b"user custom image")


    async def test_uninstall_persists_grid_intent_and_uses_owned_cache_offline(self):
        import base64
        plan = await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same")
        self.write(self.foreign, self.owned(plan))
        await self.plugin.shortcut(ACCOUNT, PROFILE, APP, "Same")
        role = "steam.grid.vertical"
        path = self.config / "grid" / f"{ID}p.png"
        original = b"owned color"
        path.write_bytes(original)
        ledger, _, binding = self.plugin._binding(ACCOUNT, PROFILE, APP)
        binding["artwork"][role] = {"revision": "fixture:false:png1",
                                    "digest": hashlib.sha256(original).hexdigest()}
        self.plugin._save(ledger)

        async def rust(*args, image=None):
            if args[:2] == ("decky", "uninstall"):
                self.assertTrue((await self.plugin.grid_states(ACCOUNT))[0]["pending"])
                return {"removed": True}
            if args == ("decky", "mute-grid"):
                self.assertEqual(image, original)
                return {"png": base64.b64encode(b"muted").decode()}
            raise main.RpcFailure("server_unavailable")

        self.plugin._rust = rust
        snapshot = self.path.read_bytes()
        self.assertTrue((await self.plugin.uninstall(PROFILE, APP, "0" * 64))["removed"])
        self.assertEqual(await self.plugin.binding(ACCOUNT, PROFILE, APP), ID)
        self.assertEqual(self.path.read_bytes(), snapshot)
        result = await self.plugin.artwork(ACCOUNT, PROFILE, APP, role, False)
        self.assertEqual(result["revision"], "fixture:true:png1")
        path.write_bytes(b"muted")
        self.assertIsNone(await self.plugin.artwork(ACCOUNT, PROFILE, APP, role, False))
        with self.assertRaises(main.RpcFailure):
            await self.plugin.artwork(ACCOUNT, PROFILE, APP, "steam.grid.horizontal", False)
        self.assertTrue((await self.plugin.grid_states(ACCOUNT))[0]["pending"])
        await self.plugin.finish_grids(ACCOUNT, PROFILE, APP)
        self.assertFalse((await self.plugin.grid_states(ACCOUNT))[0]["pending"])


def archive(version, unsafe=None):
    memory = io.BytesIO()
    with ZipFile(memory, "w") as result:
        for name in installer.EXPECTED:
            data = b"fixture"
            if name == "plugin.json":
                data = json.dumps({"api_version": 1, "name": "App Depot", "author": "App Depot", "flags": []}).encode()
            elif name == "package.json":
                data = json.dumps({"name": "app-depot-decky", "version": version}).encode()
            elif name == "bin/app-depot":
                data = b"\x7fELF\x02\x01" + bytes(12) + b"\x3e\x00"
            info = ZipInfo("AppDepot/" + name)
            info.external_attr = (stat.S_IFREG | 0o644) << 16
            result.writestr(info, data)
        if unsafe:
            result.writestr(unsafe, b"escape")
    return memory.getvalue()


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.plugins = self.root / "plugins"
        self.plugins.mkdir(mode=0o755)

    def install(self, version, hook=lambda: None, unsafe=None):
        data = archive(version, unsafe)
        installer.install(self.plugins, data, hashlib.sha256(data).hexdigest(), version, hook)

    def test_first_repeat_update_and_activation_rollback(self):
        private = self.root / "private-installed-data"
        private.write_bytes(b"immutable test data")
        self.install("one")
        manifest = self.plugins / "AppDepot/package.json"
        inode = manifest.stat().st_ino
        self.install("one")
        self.assertEqual(manifest.stat().st_ino, inode)
        self.install("two")
        self.assertEqual(json.loads(manifest.read_bytes())["version"], "two")

        def fail():
            raise RuntimeError("activation failed")

        with self.assertRaisesRegex(RuntimeError, "activation"):
            self.install("three", fail)
        self.assertEqual(json.loads(manifest.read_bytes())["version"], "two")
        self.assertEqual(private.read_bytes(), b"immutable test data")

    def test_unsafe_paths_checksums_and_missing_decky(self):
        with self.assertRaisesRegex(ValueError, "unsafe"):
            self.install("one", unsafe="AppDepot/../../escaped")
        with self.assertRaisesRegex(ValueError, "checksum"):
            installer.install(self.plugins, archive("one"), "0" * 64, "one", lambda: None)
        self.assertFalse((self.plugins / "AppDepot").exists())
        with self.assertRaisesRegex(ValueError, "Decky Loader"):
            installer.root_for(self.root, "/usr/bin/true")

    def test_loopback_download_missing_and_interrupted_package(self):
        payload = archive("one")
        digest = hashlib.sha256(payload).hexdigest()

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                if self.path == "/missing.zip":
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload if self.path == "/one.zip" else payload[:20])

        with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                url = f"http://127.0.0.1:{server.server_port}"
                self.assertEqual(installer.get_package(url + "/one.zip", digest, True), payload)
                with self.assertRaisesRegex(ValueError, "checksum"):
                    installer.get_package(url + "/one.zip", "0" * 64, True)
                with self.assertRaises(HTTPError):
                    installer.get_package(url + "/missing.zip", digest, True)
                with self.assertRaisesRegex(ValueError, "interrupted"):
                    installer.get_package(url + "/truncated.zip", digest, True)
                with self.assertRaises(ValueError):
                    installer.get_package(url + "/one.zip", digest, False)
            finally:
                server.shutdown()
                thread.join()
    def test_release_redirect_requires_exact_https_hosts_and_strips_headers(self):
        initial = ("https://github.com/viktorliho/app-depot-client/releases/download/"
                   "decky-v0.1.0-beta.1/AppDepot-Decky-v0.1.0-beta.1-x86_64.zip")
        target = "https://release-assets.githubusercontent.com/github-production-release-asset/fixture"
        redirect = installer.ReleaseRedirect(initial)
        request = Request(initial, headers={"Authorization": "Bearer fixture"})
        response = redirect.redirect_request(request, None, 302, "", {}, target)
        self.assertEqual(response.full_url, target)
        self.assertIsNone(response.get_header("Authorization"))
        self.assertIsNone(redirect.redirect_request(response, None, 302, "", {}, target))
        for changed in (target.replace("https:", "http:"), target.replace(".com/", ".com.evil/"),
                        target.replace(".com/", ".com:8443/"), target.replace(".com/", ".com@evil/")):
            self.assertIsNone(redirect.redirect_request(request, None, 302, "", {}, changed))

    def test_bootstrap_contains_pinned_metadata_and_source(self):
        source = "def desktop(*args):\n    print(args[1:])\n"
        desktop = package.desktop_entry(source, "http://127.0.0.1:8765/v1.zip", "a" * 64, "v1", True)
        self.assertIn("Exec=/usr/bin/python3 -I -c ", desktop)
        self.assertIn("# def desktop(*args):", desktop)
        self.assertIn("# SHA-256: " + "a" * 64, desktop)
