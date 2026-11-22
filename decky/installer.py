"""Installer implementation for the versioned App Depot Decky release script."""

import ctypes
import fcntl
import hashlib
import http.client
import io
import json
import os
from pathlib import Path
import pwd
import re
import shlex
import shutil
import ssl
import stat
import subprocess
import sys
import tempfile
import time
from urllib.error import HTTPError, URLError
from urllib.parse import urlparse
from urllib.request import HTTPHandler, HTTPSHandler, HTTPRedirectHandler, ProxyHandler, Request, build_opener
from zipfile import ZipFile


PACKAGE = "AppDepot"
SERVICE = "plugin_loader.service"
MAX_PACKAGE = 128 * 1024 * 1024
CONNECT_TIMEOUT = 20
IDLE_TIMEOUT = 30
MAX_ATTEMPTS = 5
READ_SIZE = 64 * 1024
RELEASE_PATH = re.compile(
    r"/viktorliho/app-depot-client/releases/download/decky-v([0-9]+\.[0-9]+\.[0-9]+-beta\.[0-9]+)/"
    r"AppDepot-Decky-v\1-x86_64\.zip"
)
EXPECTED = {"plugin.json", "package.json", "main.py", "dist/index.js", "bin/app-depot", "LICENSE"}


class ReleaseRedirect(HTTPRedirectHandler):
    def __init__(self, package_url):
        self.package_url = package_url

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        source = urlparse(req.full_url)
        target = urlparse(newurl)
        if (
            req.full_url == self.package_url
            and source.scheme == "https"
            and source.netloc == "github.com"
            and RELEASE_PATH.fullmatch(source.path)
            and not source.query and not source.fragment
            and target.scheme == "https"
            and target.netloc == "release-assets.githubusercontent.com"
            and not target.fragment
        ):
            headers = {"User-Agent": "AppDepot-Decky-Installer", "Accept-Encoding": "identity"}
            for field in ("Range", "If-range"):
                value = req.get_header(field)
                if value is not None:
                    headers[field] = value
            return Request(newurl, headers=headers)
        return None


class IdleTimeout:
    def connect(self):
        super().connect()
        self.sock.settimeout(IDLE_TIMEOUT)


class DownloadHTTPConnection(IdleTimeout, http.client.HTTPConnection):
    pass


class DownloadHTTPSConnection(IdleTimeout, http.client.HTTPSConnection):
    pass


class DownloadHTTPHandler(HTTPHandler):
    def http_open(self, req):
        return self.do_open(DownloadHTTPConnection, req)


class DownloadHTTPSHandler(HTTPSHandler):
    def https_open(self, req):
        return self.do_open(DownloadHTTPSConnection, req, context=self._context)


def response_header(response, name):
    values = response.headers.get_all(name, [])
    if len(values) > 1:
        raise ValueError("Duplicate package download header")
    return values[0] if values else None


def content_length(response):
    value = response_header(response, "Content-Length")
    if value is None:
        return None
    if not re.fullmatch(r"[0-9]{1,10}", value) or not 0 < int(value) <= MAX_PACKAGE:
        raise ValueError("Invalid package download size")
    return int(value)


def get_package(url, checksum, development, *, progress=None):
    location = urlparse(url)
    official = (location.scheme == "https" and location.netloc == "github.com"
                and RELEASE_PATH.fullmatch(location.path) and not location.query)
    local = development and location.scheme == "http" and location.hostname == "127.0.0.1"
    if (not re.fullmatch(r"[0-9a-f]{64}", checksum) or location.username or location.password
            or location.fragment or not location.path.endswith(".zip") or not (official or local)):
        raise ValueError("Invalid pinned package location")
    opener = build_opener(ProxyHandler({}), DownloadHTTPHandler(),
                          DownloadHTTPSHandler(), ReleaseRedirect(url))
    payload = bytearray()
    validator = None
    total = None
    for attempt in range(1, MAX_ATTEMPTS + 1):
        headers = {"User-Agent": "AppDepot-Decky-Installer", "Accept-Encoding": "identity"}
        offset = len(payload)
        if offset and validator and total is not None:
            headers.update({"Range": f"bytes={offset}-", "If-Range": validator})
        else:
            payload.clear()
            offset = 0
        try:
            with opener.open(Request(url, headers=headers), timeout=CONNECT_TIMEOUT) as response:
                if response_header(response, "Content-Encoding") not in (None, "identity"):
                    raise ValueError("Unexpected package transfer encoding")
                length = content_length(response)
                etag = response_header(response, "ETag")
                if response.status == 200:
                    if response_header(response, "Content-Range") is not None:
                        raise ValueError("Unexpected package Content-Range")
                    payload.clear()
                    offset = 0
                    total = length
                    validator = etag if etag and re.fullmatch(r'"[!#-~]+"', etag) else None
                elif response.status == 206 and offset:
                    value = response_header(response, "Content-Range") or ""
                    match = re.fullmatch(r"bytes ([0-9]{1,10})-([0-9]{1,10})/([0-9]{1,10})", value)
                    if (not match or tuple(map(int, match.groups())) != (offset, total - 1, total)
                            or length != total - offset or etag != validator):
                        raise ValueError("Invalid resumed package response")
                else:
                    raise ValueError("Unexpected package download response")
                if progress:
                    progress(len(payload), total, 0)
                started = time.monotonic()
                while chunk := response.read1(READ_SIZE):
                    payload.extend(chunk)
                    if len(payload) > (total if total is not None else MAX_PACKAGE):
                        raise ValueError("Package download exceeds its size limit")
                    if progress:
                        speed = (len(payload) - offset) / max(time.monotonic() - started, 0.001)
                        progress(len(payload), total, speed)
                if total is not None and len(payload) != total:
                    raise ConnectionError("Package download was interrupted")
            break
        except HTTPError as error:
            error.close()
            if error.code == 416 and offset:
                payload.clear()
                validator = None
                total = None
            elif error.code not in (408, 429, 500, 502, 503, 504):
                raise
        except URLError as error:
            if not isinstance(error.reason, OSError) or isinstance(error.reason, ssl.SSLError):
                raise
        except (ConnectionError, TimeoutError, http.client.IncompleteRead):
            pass
        if attempt == MAX_ATTEMPTS:
            raise ValueError("Package download was interrupted after retrying. Check the connection and try again.")
        delay = 2 ** (attempt - 1)
        if progress:
            progress(len(payload), total, None)
            print(f"Connection interrupted. Retrying in {delay}s ({attempt + 1}/{MAX_ATTEMPTS})…", flush=True)
        time.sleep(delay)
    verify_digest(payload, checksum)
    return bytes(payload)


def verify_digest(payload, checksum):
    if len(payload) > MAX_PACKAGE or hashlib.sha256(payload).hexdigest() != checksum:
        raise ValueError("App Depot package checksum mismatch")


def unpack(payload, destination, version):
    with ZipFile(io.BytesIO(payload)) as archive:
        files = archive.infolist()
        expected = {f"{PACKAGE}/{path}" for path in EXPECTED}
        if len(files) != len(expected) or {file.filename for file in files} != expected:
            raise ValueError("Unexpected or unsafe App Depot package paths")
        if sum(file.file_size for file in files) > MAX_PACKAGE:
            raise ValueError("App Depot package expands beyond its size limit")
        for file in files:
            mode = file.external_attr >> 16
            if stat.S_IFMT(mode) not in (0, stat.S_IFREG) or file.flag_bits & 1:
                raise ValueError("Unexpected archive entry type")
            relative = file.filename.split("/", 1)[1]
            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True, mode=0o755)
            with target.open("xb") as output, archive.open(file) as source:
                shutil.copyfileobj(source, output)
                output.flush()
                os.fsync(output.fileno())
            target.chmod(0o755 if relative == "bin/app-depot" else 0o644)
    if json.loads((destination / "plugin.json").read_text()) != {
        "api_version": 1, "name": "App Depot", "author": "App Depot", "flags": [],
    }:
        raise ValueError("Unexpected Decky plugin identity")
    package = json.loads((destination / "package.json").read_text())
    if package.get("name") != "app-depot-decky" or package.get("version") != version:
        raise ValueError("Unexpected package name or version")
    with (destination / "bin/app-depot").open("rb") as binary:
        header = binary.read(20)
    if not header.startswith(b"\x7fELF\x02\x01") or header[18:20] != b"\x3e\x00":
        raise ValueError("Package does not contain a Linux x86-64 launcher")


def root_for(home, systemctl):
    root = home / "homebrew"
    plugin_root = root / "plugins"
    environment = subprocess.run(
        [systemctl, "show", "--property=Environment", "--value", SERVICE],
        check=True, capture_output=True, text=True, timeout=15,
    ).stdout
    values = dict(part.split("=", 1) for part in shlex.split(environment) if "=" in part)
    if (values.get("PRIVILEGED_PATH") != str(root) or values.get("UNPRIVILEGED_PATH") != str(root)
            or not (root / "services/PluginLoader").is_file() or not plugin_root.is_dir()
            or root.is_symlink() or plugin_root.is_symlink()):
        raise ValueError("Decky Loader standard installation not found. Install Decky separately at https://decky.xyz/")
    return plugin_root


def exchange(first, second):
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.renameat2(-100, os.fsencode(first), -100, os.fsencode(second), 2):
        errno = ctypes.get_errno()
        raise OSError(errno, os.strerror(errno))


def install(root, payload, checksum, version, activate):
    verify_digest(payload, checksum)
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        if info.st_uid != os.geteuid() or info.st_mode & 0o022:
            raise PermissionError("Unsafe or inaccessible Decky plugin directory")
        fcntl.flock(fd, fcntl.LOCK_EX)
        pinned_root = Path(f"/proc/self/fd/{fd}")
        destination = pinned_root / PACKAGE
        if destination.is_symlink() or destination.exists() and (
            not destination.is_dir()
            or destination.stat().st_uid != os.geteuid()
            or (destination / "plugin.json").is_symlink()
            or (destination / "package.json").is_symlink()
            or json.loads((destination / "plugin.json").read_text()).get("name") != "App Depot"
        ):
            raise ValueError("Existing directory does not belong to App Depot")
        if destination.exists():
            current = json.loads((destination / "package.json").read_text())
            if current.get("name") != "app-depot-decky":
                raise ValueError("Existing Decky package identity differs")
        with tempfile.TemporaryDirectory(prefix=".app-depot-", dir=pinned_root) as scratch:
            staged = Path(scratch) / PACKAGE
            staged.mkdir(mode=0o755)
            unpack(payload, staged, version)
            if destination.exists() and all(
                (destination / name).is_file() and not (destination / name).is_symlink()
                and (destination / name).read_bytes() == (staged / name).read_bytes()
                for name in EXPECTED
            ):
                activate()
                return
            updating = destination.exists()
            if updating:
                exchange(staged, destination)
            else:
                staged.rename(destination)
            os.fsync(fd)
            try:
                if not os.path.samestat(root.stat(), info):
                    raise RuntimeError("Decky plugin directory changed during installation")
                activate()
            except BaseException:
                if updating:
                    exchange(staged, destination)
                else:
                    destination.rename(staged)
                os.fsync(fd)
                try:
                    activate()
                except Exception:
                    pass
                raise
    finally:
        os.close(fd)


def activate(systemctl):
    subprocess.run([systemctl, "restart", SERVICE], check=True, timeout=45)
    subprocess.run([systemctl, "is-active", "--quiet", SERVICE], check=True, timeout=10)


def print_download_progress():
    last = 0.0

    def update(received, total, speed):
        nonlocal last
        if speed is None:
            print(flush=True)
            last = 0.0
            return
        now = time.monotonic()
        if speed and now - last < 0.25 and received != total:
            return
        last = now
        size = f"{received / 1048576:.2f} MiB"
        if total is not None:
            size += f" / {total / 1048576:.2f} MiB"
        print(f"\rDownloading package · {size} · {speed / 1024:.0f} KiB/s          ", end="", flush=True)

    return update


def desktop(source, url, checksum, version, development):
    systemctl = "/usr/bin/systemctl"
    os.umask(0o022)
    if len(sys.argv) == 3 and sys.argv[1] == "--activate":
        user = pwd.getpwnam(sys.argv[2])
        if user.pw_uid == 0 or os.geteuid() != 0:
            raise PermissionError("Run the installer from the Steam user's Desktop Mode")
        root = root_for(Path(user.pw_dir), systemctl)
        install(root, sys.stdin.buffer.read(MAX_PACKAGE + 1), checksum, version, lambda: activate(systemctl))
        return
    if os.geteuid() == 0:
        raise PermissionError("Start this installer as the Steam user")
    try:
        print("Checking Decky Loader…", flush=True)
        root = root_for(Path.home(), systemctl)
        note = f"Install App Depot Decky {version}? The Decky service will restart. App Depot data stays in your home directory."
        if shutil.which("kdialog"):
            if subprocess.run(["kdialog", "--yesno", note], check=False).returncode:
                return
        elif input(note + " [y/N] ").lower() != "y":
            return
        print("Downloading the official package…", flush=True)
        try:
            payload = get_package(url, checksum, development, progress=print_download_progress())
        finally:
            print(flush=True)
        print("Checksum verified. Validating the archive…", flush=True)
        with tempfile.TemporaryDirectory(prefix="app-depot-check-") as scratch:
            unpack(payload, Path(scratch), version)
        print("Installing the verified package and restarting Decky…", flush=True)
        if root.stat().st_uid == os.geteuid():
            install(root, payload, checksum, version, lambda: activate(systemctl))
        else:
            elevated = "installer_source=" + repr(source) + ";exec(installer_source)"
            subprocess.run(
                ["/usr/bin/sudo", "/usr/bin/python3", "-I", "-c", elevated,
                 "--activate", pwd.getpwuid(os.getuid()).pw_name],
                input=payload, check=True, timeout=120,
            )
        message = "App Depot was installed. Return to Gaming Mode and open Decky → App Depot."
        print(message)
        if shutil.which("kdialog"):
            subprocess.run(["kdialog", "--msgbox", message], check=False)
    except Exception as error:
        message = f"App Depot installation failed: {error}"
        print(message, file=sys.stderr)
        if shutil.which("kdialog"):
            subprocess.run(["kdialog", "--error", message], check=False)
        raise SystemExit(1) from None
