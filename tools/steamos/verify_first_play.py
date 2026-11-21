"""Exercise the First Play CLI against a fresh, real loopback Dev Server."""

import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import select
import struct
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
            return "http://" + listening, setup
        time.sleep(0.05)
    raise TimeoutError("Dev Server did not start")


def run(args, root):
    namespace = root / "server-data"
    namespace.mkdir()
    setup_args = ["--namespace", str(namespace), "--instance", "fixture"]
    subprocess.run([args.server, "init-dev", *setup_args], check=True, timeout=20)
    log = root / "server.log"
    with log.open("w") as output:
        server = subprocess.Popen(
            [args.server, "serve", *setup_args, "--mode", "dev", "--bind", "127.0.0.1:0"],
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
                 "--", "--ignored"], env=env, cwd=args.client_repo, check=True, timeout=180,
            )
            subprocess.run(
                ["cargo", "test", "-p", "app-depot", "--locked", "--test", "tree_sigkill",
                 "--", "--ignored"], env=env, cwd=args.client_repo, check=True, timeout=180,
            )
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
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="app-depot-m30-") as root:
        try:
            run(args, Path(root))
        except HTTPError as error:
            raise SystemExit(f"{error.code} {error.url}: {error.read().decode(errors='replace')}")
        except subprocess.CalledProcessError as error:
            raise SystemExit(f"Process exited {error.returncode}: {error.stderr or ''}")
