#!/usr/bin/env python3
"""Replay pinned Loader install and reload code with disposable adapters."""

import argparse
import ast
import asyncio
import hashlib
from io import BytesIO
import logging
import os
from pathlib import Path
from re import sub
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
from zipfile import ZipFile


REVISIONS = {
    "v3.3.1": "ffd103b9c1a4ce5bf305e7c0998eda6e05457dad",
    "v4.0.0-pre1": "24082fe63cdcdf41993f9e1430d1405152a2c930",
}
ZIP_URL = ("https://github.com/viktorliho/app-depot-client/releases/download/"
           "decky-v0.1.0-beta.6/AppDepot-Decky-v0.1.0-beta.6-x86_64.zip")


def methods(checkout, revision, module, names, scope):
    source = subprocess.check_output(
        ["git", "-C", str(checkout), "show", f"{revision}:backend/decky_loader/{module}.py"],
        text=True,
    )
    module = ast.parse(source)
    selected = {
        member.name: member
        for node in module.body if isinstance(node, ast.ClassDef)
        for member in node.body
        if isinstance(member, (ast.FunctionDef, ast.AsyncFunctionDef)) and member.name in names
    }
    if selected.keys() != names:
        raise ValueError("Pinned Loader methods not found")
    exec(compile(ast.Module(body=list(selected.values()), type_ignores=[]),
                 f"decky-loader/{revision}", "exec"), scope)
    return SimpleNamespace(**{name: scope[name] for name in names})


async def replay(checkout, revision, root):
    events = []
    old = root / "AppDepot"
    old.mkdir()
    (old / "old").write_text("previous generation")
    data = root / "independent-state"
    data.write_text("preserved")
    package = root / "AppDepot.zip"
    with ZipFile(package, "w") as archive:
        archive.writestr("AppDepot/new", b"fixture bytes")
    payload = package.read_bytes()

    class ClientSession:
        async def __aenter__(self):
            return self

        async def __aexit__(self, *args):
            pass

        async def get(self, url, ssl):
            assert url == ZIP_URL
            async def read():
                return payload
            return SimpleNamespace(status=200, read=read)

        async def post(self, *args, **kwargs):
            return SimpleNamespace(status=200)

    scope = dict(BytesIO=BytesIO, sha256=hashlib.sha256, ZipFile=ZipFile, sub=sub,
                 logger=logging.getLogger("loader-fixture"), path=os.path, ClientSession=ClientSession,
                 get_ssl_context=lambda: None, get_loader_version=lambda: revision[:8],
                 PluginInstallType=str, PluginInstallContext=lambda url, name, version, digest: SimpleNamespace(
                     artifact=url, name=name, version=version, hash=digest),
                 time=lambda: 1, sleep=lambda _: notify())
    prompts = []
    async def notify(*args):
        if args and args[0] == "loader/add_plugin_install_prompt":
            prompts.append(args)

    async def uninstall(_name):
        events.append("uninstall")
        shutil.rmtree(old)

    browser = methods(checkout, revision, "browser", {
        "_install", "_unzip_to_plugin_dir", "request_plugin_install", "confirm_plugin_install",
    }, scope)
    def validate(state, body, name, digest):
        events.append("validate")
        return browser._unzip_to_plugin_dir(state, body, name, digest)

    import_fails = False

    async def import_plugin(*args):
        events.append("import")
        if import_fails:
            raise RuntimeError("fixture backend startup failed")

    async def download_remote(*args):
        return True

    state = SimpleNamespace(
        plugin_path=str(root), find_plugin_folder=lambda _name: "AppDepot",
        settings=SimpleNamespace(getSetting=lambda _name, default=None: [] if default is None else default),
        loader=SimpleNamespace(ws=SimpleNamespace(emit=notify), watcher=None, plugins={},
                               import_plugin=import_plugin),
        uninstall_plugin=uninstall, install_requests={},
        _download_remote_binaries_for_plugin_with_name=download_remote,
        set_plugin_dir_permissions=lambda _path: True,
    )
    state._unzip_to_plugin_dir = lambda *args: validate(state, *args)
    state._install = lambda *args: browser._install(state, *args)

    async def prompt_and_confirm(digest):
        await browser.request_plugin_install(state, ZIP_URL, "App Depot", "0.1.0-beta.6", digest, "fixture")
        request_id = prompts[-1][3]
        assert prompts[-1][4] == digest
        assert old.exists() and not events
        await browser.confirm_plugin_install(state, request_id)

    await prompt_and_confirm("0" * 64)
    assert events == ["uninstall", "validate"] and not old.exists()
    assert data.read_text() == "preserved"
    events.clear()
    old.mkdir()
    (old / "old").write_text("previous generation")
    await prompt_and_confirm(hashlib.sha256(payload).hexdigest())
    assert events == ["uninstall", "validate", "import"] and (old / "new").read_bytes() == b"fixture bytes"
    assert data.read_text() == "preserved"
    events.clear()
    (old / "old").write_text("previous generation")
    import_fails = True
    try:
        await prompt_and_confirm(hashlib.sha256(payload).hexdigest())
    except RuntimeError as error:
        assert str(error) == "fixture backend startup failed"
    else:
        raise AssertionError("Missing injected startup failure")
    assert events == ["uninstall", "validate", "import"] and not (old / "old").exists()
    assert (old / "new").exists() and data.read_text() == "preserved"

    loader = methods(checkout, revision, "loader", {"handle_plugin_backend_reload"}, {})
    queue = asyncio.Queue()
    fake = SimpleNamespace(plugins={"App Depot": SimpleNamespace(file="fixture", plugin_directory="AppDepot")},
                           reload_queue=queue)
    acknowledged = await loader.handle_plugin_backend_reload(fake, "App Depot")
    assert acknowledged is None and queue.get_nowait() == ("fixture", "AppDepot")
    print("PASS pinned Loader confirmation installs valid bytes and preserves separate state")
    print("PASS pinned Loader removes previous plugin before rejecting digest")
    print("PASS pinned Loader does not restore previous plugin on backend startup failure")
    print("PASS pinned Loader reload returns while generation is still queued")


def main():
    if os.geteuid() == 0:
        raise PermissionError("Run Loader replay as an unprivileged fixture")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checkout", type=Path)
    parser.add_argument("--loader", choices=REVISIONS, default="v3.3.1")
    args = parser.parse_args()
    logging.getLogger("loader-fixture").addHandler(logging.NullHandler())
    logging.getLogger("loader-fixture").propagate = False
    with tempfile.TemporaryDirectory(prefix="app-depot-loader-replay-") as temporary:
        asyncio.run(replay(args.checkout, REVISIONS[args.loader], Path(temporary)))


if __name__ == "__main__":
    main()
