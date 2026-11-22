"""Discover and verify official Decky releases without activating them."""

import json
import re
from pathlib import Path
from tempfile import TemporaryDirectory
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener

from app_depot_installer import get_package, unpack

REPOSITORY = "viktorliho/app-depot-client"
API = f"https://api.github.com/repos/{REPOSITORY}/releases"
RELEASES = f"https://github.com/{REPOSITORY}/releases"
BETA = r"(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})-beta\.(0|[1-9][0-9]{0,8})"
PAGE_SIZE = 20
MAX_PAGES = 5
MAX_METADATA = 2 * 1024 * 1024


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def version_key(version):
    match = re.fullmatch(BETA, version)
    if not match:
        raise ValueError("Invalid Decky beta version")
    return tuple(map(int, match.groups()))


def read_page(page):
    url = f"{API}?per_page={PAGE_SIZE}&page={page}"
    request = Request(url, headers={
        "Accept": "application/vnd.github+json",
        "User-Agent": "AppDepot-Decky",
    })
    with build_opener(ProxyHandler({}), NoRedirect()).open(request, timeout=10) as response:
        body = response.read(MAX_METADATA + 1)
    if len(body) > MAX_METADATA:
        raise ValueError("Release metadata too large")
    return json.loads(body)


def find_update(current, fetch=read_page):
    installed = version_key(current)
    candidates = {}
    for page in range(1, MAX_PAGES + 1):
        entries = fetch(page)
        if not isinstance(entries, list) or len(entries) > PAGE_SIZE:
            raise ValueError("Invalid release listing")
        for entry in entries:
            if not isinstance(entry, dict) or not isinstance(entry.get("tag_name"), str):
                raise ValueError("Invalid release entry")
            tag = entry["tag_name"]
            if not tag.startswith("decky-v") or entry.get("draft") is True:
                continue
            version = tag.removeprefix("decky-v")
            key = version_key(version)
            if key <= installed:
                continue
            if entry.get("draft") is not False or entry.get("prerelease") is not True or key in candidates:
                raise ValueError("Inconsistent Decky release")
            candidates[key] = (version, entry)
        if len(entries) < PAGE_SIZE:
            break
    else:
        raise ValueError("Release listing exceeded its page limit")
    if not candidates:
        return None

    version, entry = candidates[max(candidates)]
    tag = entry["tag_name"]
    filename = f"AppDepot-Decky-v{version}-x86_64.zip"
    assets = entry.get("assets")
    required = (filename, "AppDepot-Decky-Installer.desktop", "SHA256SUMS")
    if not isinstance(assets, list) or any(not isinstance(asset, dict) for asset in assets):
        raise ValueError("Invalid Decky assets")
    selected = {}
    for name in required:
        matching = [asset for asset in assets if asset.get("name") == name]
        url = f"{RELEASES}/download/{tag}/{name}"
        if len(matching) != 1 or matching[0].get("browser_download_url") != url or matching[0].get("state") != "uploaded":
            raise ValueError("Missing or inconsistent Decky asset")
        selected[name] = matching[0]
    asset = selected[filename]
    digest = asset.get("digest")
    notes = entry.get("body")
    if not isinstance(digest, str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", digest):
        raise ValueError("Missing GitHub SHA-256 asset digest")
    if notes is not None and not isinstance(notes, str):
        raise ValueError("Invalid release notes")
    return {
        "version": version, "notes": (notes or "")[:6000], "url": f"{RELEASES}/tag/{tag}",
        "package": asset["browser_download_url"], "sha256": digest.removeprefix("sha256:"),
    }


def verify_update(update):
    payload = get_package(update["package"], update["sha256"], False)
    with TemporaryDirectory(prefix="app-depot-update-") as scratch:
        unpack(payload, Path(scratch), update["version"])
