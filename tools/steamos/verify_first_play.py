"""Exercise the First Play CLI against a fresh, real loopback Dev Server."""

import argparse
import base64
import asyncio
import hashlib
import http.cookiejar
import importlib.util
import json
import os
from pathlib import Path
import secrets
import select
import struct
import socket
import subprocess
import tempfile
import time
import zlib
from urllib.error import HTTPError
from urllib.request import HTTPCookieProcessor, ProxyHandler, Request, build_opener


def request(opener, origin, method, path, data=None, headers=None):
    headers = {"Origin": origin, **(headers or {})}
    if isinstance(data, dict):
        data = json.dumps(data).encode()
        headers["Content-Type"] = "application/json"
    req = Request(origin + "/api/v1" + path, data=data, headers=headers, method=method)
    with opener.open(req, timeout=10) as response:
        body = response.read()
        if not body:
            return None
        return json.loads(body) if "json" in response.headers["Content-Type"] else body


def publish_fixture(opener, origin, token, program):
    def api(method, path, data=None, **headers):
        return request(opener, origin, method, path, data,
                       {"Authorization": f"Bearer {token}", **headers})

    def upload(mode, target, key, payload):
        claim = {"size": len(payload), "sha256": hashlib.sha256(payload).hexdigest()}
        args = {"mode": mode, "target": target}
        if mode == "BareFile":
            args["file"] = claim
        upload = api("POST", "/uploads", args, **{"Idempotency-Key": f"{key}-create"})
        path = f"/uploads/{upload['id']}"
        if mode == "BareTree":
            entries = [
                {"type": "Directory", "path": name}
                for name in ("bin", "data", "empty", "empty/nested")
            ]
            large = b"tree-fixture" * 65536
            members = {"bin/run.sh": payload, "data/large.bin": large}
            for name, content in members.items():
                entries.append({
                    "type": "File", "path": name, "size": len(content),
                    "sha256": hashlib.sha256(content).hexdigest(),
                    "executable": name == "bin/run.sh",
                })
            added = api("POST", f"{path}/inventory", {"entries": entries},
                        **{"Idempotency-Key": f"{key}-inventory"})
            api("POST", f"{path}/inventory/complete",
                **{"Idempotency-Key": f"{key}-inventory-complete"})
            files = [(item["fileId"], members[item["path"]]) for item in added["entries"]
                     if item["path"] in members]
        else:
            files = [(upload["file"]["fileId"], payload)]
        for file_id, content in files:
            api("PUT", f"{path}/files/{file_id}", content, **{
                "Content-Type": "application/octet-stream",
                "Content-Range": f"bytes 0-{len(content) - 1}/{len(content)}",
            })
        return api("POST", f"{path}/finalize", **{"Idempotency-Key": f"{key}-finalize"})

    tree = upload("BareTree", {"newAppName": "M3.0 Synthetic Native"}, "tree", program)
    app, release, association = (tree[key] for key in ("appId", "releaseId", "associationId"))
    upload("BareFile", {"releaseId": release}, "file", b"file-fixture" * 65536)

    def declaration(scope, ident, data):
        api("PUT", f"/{scope}/{ident}/declarations", {"data": data},
            **{"If-Match": f'"declarations-{ident}-1"'})

    declaration("releases", release, {"unknown": {"large": (1 << 64) - 1}})
    declaration("associations", association, {"appDepot": {"entryPoints": [{
        "id": "fixture", "platforms": ["linux/x86_64"],
        "target": {"type": "TreeFile", "path": "bin/run.sh"},
        "launch": {"method": "NativePortableV1", "arguments": ["literal;argument"]},
    }]}})
    art_path = f"/apps/{app}/artwork/steam.grid.vertical"
    # A synthetic 1x1 PNG, generated below without a decoder dependency.
    def chunk(kind, body):
        return (struct.pack(">I", len(body)) + kind + body
                + struct.pack(">I", zlib.crc32(kind + body)))

    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00"))
           + chunk(b"IEND", b""))
    artwork = api("PUT", art_path, png, **{
        "If-None-Match": "*", "Idempotency-Key": "m30-art", "Content-Type": "image/png",
    })
    assert artwork["width"] == 1
    assert api("GET", f"{art_path}/content") == png

    release_path = f"/releases/{release}"
    read = Request(origin + "/api/v1" + release_path, headers={"Authorization": f"Bearer {token}"})
    with opener.open(read, timeout=10) as response:
        etag = response.headers["ETag"]
    api("POST", f"{release_path}/publish", **{"Idempotency-Key": "m30-publish", "If-Match": etag})
    return app, release, association


def set_with_etag(opener, origin, token, path, data):
    headers = {"Authorization": f"Bearer {token}"}
    with opener.open(Request(origin + "/api/v1" + path, headers=headers), timeout=10) as response:
        etag = response.headers["ETag"]
    return request(opener, origin, "PATCH", path, data, {**headers, "If-Match": etag})


def enable_guest(opener, origin, token, app):
    anonymous = build_opener(ProxyHandler({}))
    assert set_with_etag(opener, origin, token, "/settings", {"anonymousReadEnabled": True})[
        "anonymousReadEnabled"
    ]
    assert request(anonymous, origin, "GET", "/apps")["items"] == []
    headers = {"Authorization": f"Bearer {token}", "Idempotency-Key": "guest-draft"}
    draft = request(opener, origin, "POST", "/apps", {"name": "Public Draft Fixture"}, headers)["id"]
    set_with_etag(opener, origin, token, f"/apps/{draft}", {"visibility": "Public"})
    # A Published release on a separate Private App stays outside the guest catalog.
    payload = b"private fixture"
    upload = request(opener, origin, "POST", "/uploads", {
        "mode": "BareFile", "target": {"newAppName": "Private Published Fixture"},
        "file": {"sha256": hashlib.sha256(payload).hexdigest(), "size": len(payload)},
    }, {**headers, "Idempotency-Key": "guest-private-upload"})
    upload_path = f"/uploads/{upload['id']}"
    request(opener, origin, "PUT", f"{upload_path}/files/{upload['file']['fileId']}", payload,
            {**headers, "Content-Type": "application/octet-stream", "Content-Range": f"bytes 0-{len(payload)-1}/{len(payload)}"})
    private = request(opener, origin, "POST", upload_path + "/finalize",
                      headers={**headers, "Idempotency-Key": "guest-private-finalize"})
    release_path = f"/releases/{private['releaseId']}"
    with opener.open(Request(origin + "/api/v1" + release_path, headers=headers), timeout=10) as response:
        release_etag = response.headers["ETag"]
    request(opener, origin, "POST", release_path + "/publish",
            headers={**headers, "If-Match": release_etag, "Idempotency-Key": "guest-private-publish"})
    set_with_etag(opener, origin, token, f"/apps/{app}", {"visibility": "Public"})
    assert {row["id"] for row in request(anonymous, origin, "GET", "/apps")["items"]} == {app}
    for path in (f"/apps/{draft}", f"/apps/{private['appId']}", f"{release_path}/declaration-snapshot",
                 f"/artifacts/{private['artifactId']}/content"):
        try:
            request(anonymous, origin, "GET", path)
        except HTTPError as error:
            assert error.code == 404, path
        else:
            raise AssertionError(f"Guest reached {path}")
    return anonymous


def wait_ready(server, log):
    for _ in range(200):
        if server.poll() is not None:
            raise RuntimeError(f"Dev Server exited with {server.returncode}")
        events = []
        for line in log.read_text().splitlines():
            try:
                events.append(json.loads(line))
            except ValueError:
                continue
        listening = next((event["bind"] for event in events
                          if event.get("event") == "listening"), None)
        if listening:
            setup = next(event["token"] for event in events if event.get("event") == "initial_setup")
            return "http://" + listening.replace("0.0.0.0:", "127.0.0.1:"), setup
        time.sleep(0.05)
    raise TimeoutError("Dev Server did not start")


def verify_decky(args, root, origin, token, app, server, opener, server_origin):
    """Exercise the packaged backend, OS credential store and wrapper."""
    package_root = root / "plugin"
    (package_root / "bin").mkdir(parents=True)
    (package_root / "bin" / "app-depot").symlink_to(args.client.resolve())
    package = (args.decky_plugin.parent / "package.json").read_bytes()
    (package_root / "package.json").write_bytes(package)
    plugin_home = root / "steam-user"
    plugin_home.mkdir()
    os.environ["DECKY_USER_HOME"] = str(plugin_home)
    os.environ["DECKY_PLUGIN_DIR"] = str(package_root)
    os.environ["XDG_DATA_HOME"] = str(plugin_home / "data")
    spec = importlib.util.spec_from_file_location("app_depot_backend", args.decky_plugin)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    async def configure_backend():
        backend = module.Plugin()
        await backend._main()
        try:
            status = await backend.status()
            assert status["profiles"] == []
            assert status["version"] == json.loads(package)["version"] != status["coreVersion"]
            if args.decky_lan_host:
                try:
                    await backend.test(origin, token)
                except module.RpcFailure as error:
                    assert error.code == "http_consent_required"
                else:
                    raise AssertionError("LAN HTTP connected without consent")
            allow_http = bool(args.decky_lan_host)
            assert await backend.request("test", origin, "", allow_http) == {
                "ok": False, "code": "anonymous_reading_disabled",
            }
            assert await backend.request("test", origin, "invalid-fixture-token", allow_http) == {
                "ok": False, "code": "invalid_token",
            }
            assert (await backend.test(origin, token, allow_http))["connected"]
            profile = await backend.configure(origin, token, allow_http)
            assert (await backend.status())["profiles"][0]["authenticated"]
            rows = await backend.catalog(profile, True)
            assert any(row["id"] == app and not row["downloaded"] for row in rows)
            media = await backend._rust("decky", "artwork", profile, app, "steam.grid.vertical", "muted", "")
            assert media["revision"].endswith(":true:png1")
            color = await backend._rust("decky", "artwork", profile, app, "steam.grid.vertical", "color", "")
            offline_muted = await backend._rust("decky", "mute-grid", image=base64.b64decode(color["png"], validate=True))
            assert offline_muted["png"] == media["png"]
            unchanged = await backend._rust("decky", "artwork", profile, app, "steam.grid.vertical", "muted", media["revision"])
            assert unchanged == {"revision": media["revision"]}
            return profile
        finally:
            await backend._unload()

    profile = asyncio.run(configure_backend())
    state = root / "launcher-state"
    env = dict(os.environ, HOME=str(plugin_home), XDG_STATE_HOME=str(state))
    env.pop("APP_DEPOT_TOKEN", None)
    command = [args.client, "decky", "play", profile, app]
    play = subprocess.run(command, check=True, timeout=45, env=env, capture_output=True, text=True)
    assert "m30 fixture executed" in play.stdout
    trace = json.loads((state / "app-depot-decky/last-launch.json").read_text())
    assert set(trace) == {"pid", "exitCode", "stage"}
    assert trace["stage"] == "exited" and trace["exitCode"] == 0

    async def check_after_reload():
        backend = module.Plugin()
        await backend._main()
        try:
            rows = await backend.catalog(profile, False)
            assert any(row["id"] == app and row["downloaded"] and row["installations"] for row in rows)
        finally:
            await backend._unload()

    asyncio.run(check_after_reload())
    anonymous = enable_guest(opener, server_origin, token, app)
    assert request(anonymous, server_origin, "GET", f"/apps/{app}/artwork/steam.grid.vertical/content")
    guest_home = root / "steam-guest"
    guest_home.mkdir()

    async def configure_guest():
        backend = module.Plugin()
        await backend._main()
        try:
            allow_http = bool(args.decky_lan_host)
            assert await backend.request("configure", origin, "", allow_http) == {
                "ok": False, "code": "saved_credentials",
            }
            assert (await backend.status())["profiles"][0]["authenticated"]
        finally:
            await backend._unload()
        os.environ["DECKY_USER_HOME"] = str(guest_home)
        os.environ["XDG_DATA_HOME"] = str(guest_home / "data")
        guest = module.Plugin()
        await guest._main()
        try:
            result = await guest.request("test", origin, "", allow_http)
            assert result["ok"] and result["value"]["guest"] and result["value"]["appCount"] == 1
            guest_profile = await guest.configure(origin, "", allow_http)
            assert not (await guest.status())["profiles"][0]["authenticated"]
            assert [row["id"] for row in await guest.catalog(guest_profile, True)] == [app]
            image = await guest._rust("decky", "artwork", guest_profile, app, "steam.grid.vertical", "muted", "")
            assert image["png"]
            return guest_profile
        finally:
            await guest._unload()

    guest_profile = asyncio.run(configure_guest())
    guest_env = dict(os.environ, HOME=str(guest_home), XDG_STATE_HOME=str(root / "guest-state"),
                     APP_DEPOT_TOKEN="invalid-fixture-token")
    guest_command = [args.client, "decky", "play", guest_profile, app]
    first = subprocess.run(guest_command, check=True, timeout=45, env=guest_env, capture_output=True, text=True)
    assert "m30 fixture executed" in first.stdout
    os.environ["DECKY_USER_HOME"] = str(plugin_home)
    os.environ["XDG_DATA_HOME"] = str(plugin_home / "data")

    async def uninstall_backend(expected=None):
        backend = module.Plugin()
        await backend._main()
        try:
            items = [row for row in await backend.installed() if row["profile"] == profile and row["id"] == app]
            assert len(items) == 1 and items[0]["runtimeBytes"] > 0 and items[0]["downloadBytes"] > 0
            item = items[0]
            if expected:
                assert item["blocked"]
                assert await backend.request("uninstall", profile, app, item["revision"]) == {
                    "ok": False, "code": expected,
                }
                return
            assert await backend.request("uninstall", profile, app, "0" * 64) == {
                "ok": False, "code": "uninstall_stale",
            }
            assert (await backend.request("uninstall", profile, app, item["revision"]))["value"]["removed"]
            assert await backend.request("uninstall", profile, app, item["revision"]) == {
                "ok": False, "code": "uninstall_stale",
            }
            assert not await backend.installed()
            rows = await backend.catalog(profile, False)
            assert any(row["id"] == app and not row["downloaded"] and not row["installations"] for row in rows)
            assert (await backend.status())["profiles"][0]["authenticated"]
        finally:
            await backend._unload()

    asyncio.run(uninstall_backend())
    again = subprocess.run(command, check=True, timeout=45, env=env, capture_output=True, text=True)
    assert "m30 fixture executed" in again.stdout
    print("PASS stale selection, complete local removal, repeat removal refusal and First Play reacquisition")
    server.terminate()
    server.wait(timeout=20)
    env.pop("DISPLAY", None)
    env.pop("WAYLAND_DISPLAY", None)
    guest_env.pop("DISPLAY", None)
    guest_env.pop("WAYLAND_DISPLAY", None)
    guest_offline = subprocess.run(guest_command, check=True, timeout=15, env=guest_env, capture_output=True, text=True)
    assert "m30 fixture executed" in guest_offline.stdout
    print("PASS guest access, private and draft isolation, disabled access, invalid token, artwork and guest offline replay")
    offline = subprocess.run(command, check=True, timeout=15, env=env, capture_output=True, text=True)
    assert "m30 fixture executed" in offline.stdout

    # Replace only this disposable writable runtime. The immutable object and
    # declaration remain intact, so Play must permit the user modification.
    runtime = next((plugin_home / ".appdepot/apps").glob("*/bin/run.sh"))
    source = root / "exit-status.c"
    source.write_text("int main(void) { return 17; }\n")
    subprocess.run(["cc", str(source), "-o", str(runtime)], check=True, timeout=20)
    exited = subprocess.run(command, timeout=10, env=env, capture_output=True, text=True)
    assert exited.returncode == 17

    source.write_text(
        '#include <stdio.h>\n#include <unistd.h>\n'
        'int main(void) { puts("signal fixture started"); fflush(stdout); sleep(30); return 0; }\n'
    )
    subprocess.run(["cc", str(source), "-o", str(runtime)], check=True, timeout=20)
    with subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          text=True) as wrapper:
        try:
            ready, _, _ = select.select([wrapper.stdout], [], [], 5)
            assert ready and wrapper.stdout.readline().strip() == "signal fixture started"
            assert wrapper.poll() is None
            asyncio.run(uninstall_backend("uninstall_busy"))
            wrapper.terminate()
            assert wrapper.wait(timeout=5) == 143
        finally:
            if wrapper.poll() is None:
                wrapper.kill()
    asyncio.run(uninstall_backend())
    assert not list((plugin_home / ".appdepot/apps").iterdir())
    print("PASS offline complete removal and active game refusal")
    print("PASS Decky backend configure/reload, Secret Service, X11 First Play, "
          "display-free offline replay, child exit status and SIGTERM supervision")


def run(args, root):
    if args.decky_lan_host and (
        not args.decky_plugin or [name for _, name in socket.if_nameindex()] != ["lo"]
    ):
        raise ValueError("LAN test requires a Decky plugin and an isolated network namespace")
    bind = "0.0.0.0:0" if args.decky_lan_host else "127.0.0.1:0"
    namespace = root / "server-data"
    namespace.mkdir()
    setup_args = ["--namespace", str(namespace), "--instance", "fixture"]
    subprocess.run([args.server, "init-dev", *setup_args], check=True, timeout=20)
    log = root / "server.log"
    with log.open("w") as output:
        server = subprocess.Popen(
            [args.server, "serve", *setup_args, "--mode", "dev", "--bind", bind],
            stdout=output, stderr=output,
        )
    try:
        origin, setup = wait_ready(server, log)
        opener = build_opener(ProxyHandler({}), HTTPCookieProcessor(http.cookiejar.CookieJar()))
        credentials = {"username": "admin", "password": secrets.token_urlsafe(32)}
        request(opener, origin, "POST", "/setup", {"token": setup, **credentials})
        session = request(opener, origin, "POST", "/auth/sessions", credentials)
        token = request(opener, origin, "PUT", "/me/token", {},
                        {"X-CSRF-Token": session["csrfToken"]})["token"]
        binary = root / "fixture"
        source = root / "fixture.c"
        source.write_text(
            '#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n'
            '#include <unistd.h>\n'
            'int main(int argc, char **argv) {\n'
            ' if (argc != 2 || strcmp(argv[1], "literal;argument") || getenv("APP_DEPOT_TOKEN")) return 17;\n'
            ' puts("m30 fixture started"); fflush(stdout); sleep(1);\n'
            ' puts("m30 fixture executed"); return 0;\n}\n'
        )
        subprocess.run(["cc", "-O2", str(source), "-o", str(binary)], check=True, timeout=20)
        app, release, association = publish_fixture(opener, origin, token, binary.read_bytes())

        def invocation(command, authenticated, library):
            env = dict(os.environ)
            env.pop("APP_DEPOT_TOKEN", None)
            if authenticated:
                env["APP_DEPOT_TOKEN"] = token
            return [args.client, "--library-data", str(root / library), *command], env

        def client(*command, authenticated=True, library="client-library"):
            argv, env = invocation(command, authenticated, library)
            return subprocess.run(argv, env=env, check=True, capture_output=True,
                                  text=True, timeout=30)

        profile = json.loads(client("--json", "profile", "add", "M3.0 fixture", origin).stdout)["id"]
        client("--profile", profile, "sync")
        missing_profile = json.loads(client("--json", "profile", "add", "M3.0 missing", origin,
                                            library="missing-library").stdout)["id"]
        client("--profile", missing_profile, "sync", library="missing-library")

        # A Server-side rename must update the cache without changing App identity.
        app_url = f"{origin}/api/v1/apps/{app}"
        authorization = {"Authorization": f"Bearer {token}"}
        with opener.open(Request(app_url, headers=authorization), timeout=10) as response:
            app_etag = response.headers["ETag"]
        new_name = "M3.0 Renamed Native"
        request(opener, origin, "PATCH", f"/apps/{app}", {"name": new_name},
                {**authorization, "If-Match": app_etag})
        client("--profile", profile, "sync")
        apps = json.loads(client("--json", "--profile", profile, "cached-apps").stdout)
        assert any(row["value"]["id"] == app and row["value"]["name"] == new_name for row in apps)

        play = ("--profile", profile, "first-play", app, release, association, "fixture")
        argv, env = invocation(play, True, "client-library")
        with subprocess.Popen(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              text=True) as wrapper:
            try:
                ready, _, _ = select.select([wrapper.stdout], [], [], 20)
                assert ready and wrapper.stdout.readline().strip() == "m30 fixture started"
                assert wrapper.poll() is None, "wrapper exited while its child was active"
                stdout, stderr = wrapper.communicate(timeout=10)
                assert wrapper.returncode == 0, stderr
                assert "m30 fixture executed" in stdout and "downloading Artifact" in stderr
            finally:
                if wrapper.poll() is None:
                    wrapper.kill()
        installs = json.loads(client("--json", "--profile", profile, "runtime", "list").stdout)
        assert len(installs) == 1
        runtime_program = next((root / "client-library" / "apps").glob("*/bin/run.sh"))
        # Appended bytes change the executable identity without changing ELF execution.
        with runtime_program.open("ab") as executable:
            executable.write(b"local user modification")
        jobs_before = json.loads(client("--json", "--profile", profile, "download", "jobs").stdout)
        assert len(jobs_before) == 1 and jobs_before[0]["state"] == "Completed"

        if args.client_repo:
            env = dict(os.environ, APP_DEPOT_TEST_SERVER_URL=origin, APP_DEPOT_TEST_TOKEN=token,
                       APP_DEPOT_TEST_APP_ID=app, APP_DEPOT_TEST_RELEASE_ID=release,
                       APP_DEPOT_TEST_ASSOCIATION_COUNT="2", APP_DEPOT_TOKEN=token,
                       APP_DEPOT_TEST_TREE_ASSOCIATION_ID=association)
            subprocess.run(
                ["cargo", "test", "-p", "app-depot-core", "--locked", "--test", "real_server",
                 "--", "--ignored", "--skip", "installs_real_standard_plan_and_reuses_downloads"],
                env=env, cwd=args.client_repo, check=True, timeout=180,
            )
            subprocess.run(
                ["cargo", "test", "-p", "app-depot", "--locked", "--test", "tree_sigkill",
                 "--", "--ignored"], env=env, cwd=args.client_repo, check=True, timeout=180,
            )
            # The portable launch fixture intentionally has no installation manifest.
            # Exercise the Standard test against an explicit declaration on this Dev Server.
            release_path = f"/releases/{release}/declarations"
            def replace_release(data):
                with opener.open(Request(origin + "/api/v1" + release_path,
                                         headers=authorization), timeout=10) as response:
                    etag = response.headers["ETag"]
                request(opener, origin, "PUT", release_path, {"data": data},
                        {**authorization, "If-Match": etag})
            original = request(opener, origin, "GET", release_path, headers=authorization)["data"]
            entry = request(opener, origin, "GET", f"/associations/{association}/declarations",
                            headers=authorization)
            associations = request(opener, origin, "GET", f"/releases/{release}/artifacts",
                                   headers=authorization)["items"]
            file_id = next(a["id"] for a in associations
                           if a["id"] != association)
            replace_release({**original, "appDepot": {"installation": {"version": 1,
                "payload": [{"associationId": association, "destination": ""},
                            {"associationId": file_id, "destination": "extra.dat"}],
                "launch": {"associationId": association, "entryPointId": "fixture",
                           "revisionId": entry["revisionId"], "destination": "bin/run.sh"}}}})
            subprocess.run(
                ["cargo", "test", "-p", "app-depot-core", "--locked", "--test", "real_server",
                 "installs_real_standard_plan_and_reuses_downloads", "--", "--ignored"],
                env=env, cwd=args.client_repo, check=True, timeout=180,
            )
            replace_release(original)
        if args.decky_plugin:
            decky_origin = origin
            if args.decky_lan_host:
                decky_origin = origin.replace("127.0.0.1", args.decky_lan_host)
            verify_decky(args, root, decky_origin, token, app, server, opener, origin)
        else:
            server.terminate()
            server.wait(timeout=20)
        offline = client(*play, authenticated=False)
        assert "m30 fixture executed" in offline.stdout and "downloading Artifact" not in offline.stderr
        jobs_after = json.loads(client("--json", "--profile", profile, "download", "jobs",
                                       authenticated=False).stdout)
        assert jobs_after == jobs_before
        try:
            client("--profile", missing_profile, *play[2:], authenticated=False,
                   library="missing-library")
        except subprocess.CalledProcessError as error:
            assert error.returncode == 5 and "m30 fixture executed" not in error.stdout
        else:
            raise AssertionError("uninstalled App launched with Server stopped")
        print("PASS authenticated catalog, artwork bytes, verified Tree First Play, "
              "modified offline replay and clear missing-content failure")

    finally:
        if server.poll() is None:
            server.kill()
            server.wait(timeout=10)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--client", type=Path, required=True)
    parser.add_argument("--client-repo", type=Path, help="also run existing real Server conformance tests")
    parser.add_argument("--decky-plugin", type=Path,
                        help="also exercise the real Decky RPC backend and native wrapper under X11")
    parser.add_argument("--decky-lan-host", choices=["192.168.77.1"],
                        help="test explicit LAN HTTP via an address on the isolated loopback interface")
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="app-depot-m30-") as root:
        try:
            run(args, Path(root))
        except HTTPError as error:
            raise SystemExit(f"{error.code} {error.url}: {error.read().decode(errors='replace')}")
        except subprocess.CalledProcessError as error:
            raise SystemExit(f"Process exited {error.returncode}: {error.stderr or ''}")
