"""Create a deterministic Decky ZIP and a pinned single-file Desktop installer."""

import argparse
import hashlib
import json
import re
from pathlib import Path
import stat
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo


SCRIPT_LIMIT = 64 * 1024
VERSION = r"[0-9]+\.[0-9]+\.[0-9]+-beta\.[0-9]+"
RELEASE_BASE = "https://github.com/viktorliho/app-depot-client/releases/download"


def desktop_entry(url, digest, development):
    location = (
        rf"AppDepot-Decky-Installer-v{VERSION}\.py"
        if development else
        rf"{re.escape(RELEASE_BASE)}/decky-v({VERSION})/AppDepot-Decky-Installer-v\1\.py"
    )
    if not re.fullmatch(location, url) or not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError("Invalid pinned installer script location or digest")
    # Pass the pins as plain Python arguments to avoid nested Desktop Entry quoting.
    redirect = (
        "req.full_url==url and urlsplit(target)[:2]=="
        "('https','release-assets.githubusercontent.com') and not urlsplit(target).fragment"
        if not development else "False"
    )
    command = (
        "import sys,hashlib; from urllib.request import build_opener,ProxyHandler,"
        "HTTPRedirectHandler,Request; from urllib.parse import urlsplit; "
        "url,digest=sys.argv[1:]; redirects=HTTPRedirectHandler(); "
        "redirects.redirect_request=lambda req,fp,code,msg,headers,target:"
        f" Request(target) if {redirect} else None; "
        "print('Downloading App Depot installer…',flush=True); "
        "response=build_opener(ProxyHandler({}),redirects).open(url,timeout=20); "
        f"source=response.read({SCRIPT_LIMIT + 1}); "
        f"(0<len(source)<={SCRIPT_LIMIT} and hashlib.sha256(source).hexdigest()==digest)"
        " or sys.exit('Installer SHA-256 verification failed'); "
        "exec(compile(source,'App Depot installer','exec'),{'installer_source':source})"
    )
    arguments = f"{url} {digest}"
    if development:
        # %k is expanded by the desktop launcher as a single argument.
        command = (
            "import sys,hashlib,traceback; from pathlib import Path; "
            "from urllib.parse import urlsplit,unquote; "
            "log=Path.home()/'AppDepot-Decky-Installer.log'; "
            "sys.excepthook=lambda *exc: (log.write_text(''.join(traceback.format_exception(*exc))),"
            "sys.__excepthook__(*exc)); "
            "location,name,digest=sys.argv[1:]; "
            "directory=Path(unquote(urlsplit(location).path) if location.startswith('file:') else location).parent; "
            f"source=(directory/name).open('rb').read({SCRIPT_LIMIT + 1}); "
            f"0<len(source)<={SCRIPT_LIMIT} and hashlib.sha256(source).hexdigest()==digest"
            " or (_ for _ in ()).throw(ValueError('Installer SHA-256 verification failed')); "
            "exec(compile(source,'App Depot installer','exec'),"
            "{'installer_source':source,'development_directory':directory})"
        )
        arguments = f"%k {url} {digest}"
    return ('#!/usr/bin/env xdg-open\n[Desktop Entry]\nType=Application\n'
            'Name=Install App Depot for Decky\nIcon=system-software-install\n'
            'Terminal=true\nTerminalOptions=--noclose\n'
            f'Exec=/usr/bin/python3 -I -c "{command}" {arguments}\n'
            f'\n# Script: {url}\n# Script SHA-256: {digest}\n')


def build(binary, out, base_url, development, github_release_tag=None):
    source = Path(__file__).parent
    version = json.loads((source / "package.json").read_text())["version"]
    if not re.fullmatch(VERSION, version):
        raise ValueError("Invalid Decky beta version")
    if github_release_tag:
        if development or github_release_tag != f"decky-v{version}":
            raise ValueError("Public Decky beta tag must match package.json")
        base_url = f"{RELEASE_BASE}/{github_release_tag}"
        name = f"AppDepot-Decky-v{version}-x86_64.zip"
    else:
        name = f"AppDepot-Decky-{version}-linux-x86_64.zip"
    paths = {
        "plugin.json": source / "plugin.json",
        "package.json": source / "package.json",
        "main.py": source / "main.py",
        "dist/index.js": source / "dist/index.js",
        "bin/app-depot": binary,
        "LICENSE": source.parent / "LICENSE",
    }
    out.mkdir(parents=True, exist_ok=True)
    package = out / name
    with ZipFile(package, "w", compresslevel=9) as archive:
        for name, real in sorted(paths.items()):
            item = ZipInfo(f"AppDepot/{name}", (2000, 1, 1, 0, 0, 0))
            item.create_system = 3
            item.compress_type = ZIP_DEFLATED
            item.external_attr = (stat.S_IFREG | (0o755 if name.startswith("bin/") else 0o644)) << 16
            archive.writestr(item, real.read_bytes(), compresslevel=9)
    digest = hashlib.sha256(package.read_bytes()).hexdigest()
    base_url = base_url.rstrip("/")
    if not github_release_tag and (
        not development or not re.fullmatch(r"http://127\.0\.0\.1:[0-9]+", base_url)
    ):
        raise ValueError("Use an official release tag or an explicit development-loopback URL")
    script = out / f"AppDepot-Decky-Installer-v{version}.py"
    package_url = package.name if development else f"{base_url}/{package.name}"
    source_bytes = (source / "installer.py").read_bytes() + (
        f"\ndesktop(installer_source, {package_url!r}, {digest!r}, {version!r}, {development!r})\n"
    ).encode("utf-8")
    if len(source_bytes) > SCRIPT_LIMIT:
        raise ValueError("Installer exceeds the bootstrap download limit")
    script.write_bytes(source_bytes)
    script_digest = hashlib.sha256(source_bytes).hexdigest()
    desktop = out / "AppDepot-Decky-Installer.desktop"
    desktop.write_text(desktop_entry(script.name if development else f"{base_url}/{script.name}", script_digest, development), encoding="utf-8")
    (out / "SHA256SUMS").write_text(
        f"{digest}  {package.name}\n{script_digest}  {script.name}\n"
    )
    desktop.chmod(0o755)
    print(package)
    print(script)
    print(desktop)
    print(digest)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--base-url", default="http://127.0.0.1:8765")
    parser.add_argument("--development", "--development-loopback", dest="development_loopback", action="store_true")
    parser.add_argument("--github-release-tag")
    args = parser.parse_args()
    build(args.binary, args.out, args.base_url, args.development_loopback, args.github_release_tag)
