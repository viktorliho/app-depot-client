import asyncio
import hashlib
import io
import ssl
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import ANY, MagicMock, patch
from urllib.error import HTTPError, URLError
from urllib.request import Request

import installer
import main
from test_decky import archive

with patch.dict(sys.modules, {"app_depot_installer": installer}):
    sys.path.insert(0, str(Path(__file__).parent / "py_modules"))
    try:
        import decky_updates as updates
    finally:
        sys.path.pop(0)


def release(version, payload=b"fixture"):
    tag = f"decky-v{version}"
    names = (f"AppDepot-Decky-v{version}-x86_64.zip", "AppDepot-Decky-Installer.desktop", "SHA256SUMS")
    return {
        "tag_name": tag, "draft": False, "prerelease": True, "body": "Release notes",
        "assets": [{
            "name": name, "browser_download_url": f"{updates.RELEASES}/download/{tag}/{name}",
            "state": "uploaded", "digest": "sha256:" + hashlib.sha256(payload).hexdigest(),
        } for name in names],
    }


class UpdateTests(unittest.TestCase):
    def test_choose_highest_and_ignore_desktop_draft_and_downgrade(self):
        old = release("0.1.0-beta.2")
        draft = dict(release("1.0.0-beta.1"), draft=True)
        desktop = {"tag_name": "v3.0.0", "prerelease": False}
        self.assertIsNone(updates.find_update("0.1.0-beta.3", lambda _: [old, draft, desktop]))
        latest = release("0.1.0-beta.10")
        first_page = [desktop] * updates.PAGE_SIZE
        listing = {1: first_page, 2: [latest, release("0.1.0-beta.9")]}
        self.assertEqual(updates.find_update("0.1.0-beta.3", listing.__getitem__)["version"], "0.1.0-beta.10")

    def test_explicit_system_ca_etag_and_up_to_date(self):
        response = MagicMock()
        response.__enter__.return_value = response
        response.headers = {"ETag": '"fixture"'}
        response.read.return_value = json.dumps([release("0.1.0-beta.4"), {"tag_name": "v0.1.3"}]).encode()
        opener = MagicMock()
        opener.open.side_effect = [response, HTTPError(updates.API, 304, "", {}, io.BytesIO())]
        fetch = updates.ReleasePages()
        with patch.object(updates, "build_opener", return_value=opener) as build, patch.object(
            updates, "monotonic", side_effect=[0, 1, 61, 61],
        ):
            self.assertIsNone(updates.find_update("0.1.0-beta.4", fetch))
            self.assertIsNone(updates.find_update("0.1.0-beta.4", fetch))
            self.assertIsNone(updates.find_update("0.1.0-beta.4", fetch))
        request = opener.open.call_args.args[0]
        self.assertEqual(request.get_header("If-none-match"), '"fixture"')
        self.assertEqual(opener.open.call_count, 2)
        context = build.call_args.args[1]._context
        self.assertTrue(context.check_hostname)
        self.assertEqual(context.verify_mode, ssl.CERT_REQUIRED)

    def test_missing_duplicate_untrusted_and_malformed_metadata(self):
        for field, value in (
            ("digest", None), ("browser_download_url", "http://github.com/example.zip"),
            ("state", "new"),
        ):
            with self.subTest(field=field):
                entry = release("0.1.0-beta.4")
                entry["assets"][0][field] = value
                with self.assertRaises(ValueError):
                    updates.find_update("0.1.0-beta.3", lambda _: [entry])
        for entries in (
            {}, [dict(release("0.1.0-beta.4"), assets=[])],
            [dict(release("0.1.0-beta.4"), prerelease=False)],
            [dict(release("0.1.0-beta.4"), tag_name="decky-v01.1.0-beta.4")],
            [release("0.1.0-beta.4")] * 2,
        ):
            with self.subTest(entries=entries), self.assertRaises(ValueError):
                updates.find_update("0.1.0-beta.3", lambda _: entries)

    def test_page_limit_and_metadata_redirect(self):
        with self.assertRaises(ValueError):
            updates.find_update("0.1.0-beta.3", lambda _: [{"tag_name": "v1"}] * updates.PAGE_SIZE)
        req = Request(updates.API)
        self.assertIsNone(updates.NoRedirect().redirect_request(req, None, 302, "", {}, updates.API + "/other"))

    def test_verify_download_and_archive_then_discard(self):
        version = "0.1.0-beta.4"
        for unsafe in (None, "AppDepot/../escape"):
            with self.subTest(unsafe=unsafe):
                payload = archive(version, unsafe)
                candidate = updates.find_update("0.1.0-beta.3", lambda _: [release(version, payload)])
                with patch.object(updates, "get_package", return_value=payload) as download:
                    if unsafe:
                        with self.assertRaises(ValueError):
                            updates.verify_update(candidate)
                    else:
                        self.assertIsNone(updates.verify_update(candidate))
                        download.assert_called_once_with(candidate["package"], candidate["sha256"], False, context=ANY)
                with patch.object(updates, "get_package", side_effect=ValueError("wrong digest")):
                    with self.assertRaises(ValueError):
                        updates.verify_update(candidate)


class UpdateRpcTests(unittest.IsolatedAsyncioTestCase):
    async def test_concurrent_check_retry_reload_and_user_data(self):
        with tempfile.TemporaryDirectory() as scratch, patch.dict(sys.modules, {"decky_updates": updates}):
            root = Path(scratch)
            (root / "package.json").write_text(json.dumps({"name": "app-depot-decky", "version": "0.1.0-beta.3"}))
            data = root / "bindings.json"
            data.write_text("persistent test data")
            plugin = main.Plugin()
            plugin.plugin = root
            plugin.update_lock = asyncio.Lock()
            plugin.update_candidate = None
            plugin.release_pages = None
            candidate = updates.find_update("0.1.0-beta.3", lambda _: [release("0.1.0-beta.4")])
            loop = asyncio.get_running_loop()
            started = asyncio.Event()
            resume = threading.Event()

            def slow_check(version, _fetch):
                self.assertEqual(version, "0.1.0-beta.3")
                loop.call_soon_threadsafe(started.set)
                if not resume.wait(3):
                    raise TimeoutError
                return candidate

            with patch.object(updates, "find_update", side_effect=slow_check):
                first = asyncio.create_task(plugin.request("check_update"))
                try:
                    await asyncio.wait_for(started.wait(), timeout=2)
                    self.assertEqual(await plugin.request("verify_update"), {"ok": False, "code": "update_busy"})
                finally:
                    resume.set()
                checked = await first
            self.assertTrue(checked["ok"])
            self.assertEqual(set(checked["value"]), {"version", "notes", "url"})

            with patch.object(updates, "verify_update", side_effect=[OSError("interrupted"), None]):
                self.assertEqual(await plugin.request("verify_update"),
                                 {"ok": False, "code": "update_verification_failed"})
                self.assertEqual(await plugin.request("verify_update"), {"ok": True, "value": None})
            with patch.object(updates, "find_update", side_effect=OSError("network failure")):
                self.assertEqual(await plugin.request("check_update"),
                                 {"ok": False, "code": "update_network_failed"})
            self.assertEqual(await plugin.request("verify_update"), {"ok": False, "code": "update_check_failed"})
            reloaded = main.Plugin()
            reloaded.plugin = root
            reloaded.update_lock = asyncio.Lock()
            reloaded.update_candidate = None
            reloaded.release_pages = None
            self.assertEqual(await reloaded.request("verify_update"), {"ok": False, "code": "update_check_failed"})
            rate_limit = HTTPError(updates.API, 403, "limited", {"X-RateLimit-Remaining": "0"}, None)
            with patch.object(updates, "find_update", side_effect=rate_limit):
                self.assertEqual(await reloaded.request("check_update"), {"ok": False, "code": "update_rate_limited"})
            for error, code in (
                (URLError(ssl.SSLCertVerificationError("no trusted issuer")), "update_tls_failed"),
                (URLError(TimeoutError("timed out")), "update_network_failed"),
                (ValueError("malformed release"), "update_metadata_invalid"),
                (HTTPError(updates.API, 502, "upstream", {}, None), "update_http_failed"),
            ):
                with patch.object(updates, "find_update", side_effect=error):
                    self.assertEqual(await reloaded.request("check_update"), {"ok": False, "code": code})
            async def status(*args):
                return {"version": "0.1.3"}
            reloaded._rust = status
            self.assertEqual((await reloaded.request("status"))["value"],
                             {"version": "0.1.0-beta.3", "coreVersion": "0.1.3"})
            self.assertEqual(data.read_text(), "persistent test data")
