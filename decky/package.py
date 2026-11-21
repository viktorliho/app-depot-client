"""Create a deterministic Decky ZIP and a pinned single-file Desktop installer."""

import argparse
import hashlib
import json
from pathlib import Path
import stat
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo


def desktop_entry(source, url, digest, version, development):
    args = ",".join(map(repr, (source, url, digest, version, development)))
    command = "exec(" + repr(source) + ");desktop(" + args + ")"
    for original, escaped in (("\\", "\\\\\\\\"), ('"', '\\\\"'), ("$", "\\\\$"), ("`", "\\\\`"), ("%", "%%")):
        command = command.replace(original, escaped)
    commentary = "\n".join("# " + line for line in source.splitlines())
    return ('#!/usr/bin/env xdg-open\n[Desktop Entry]\nType=Application\n'
            'Name=Install App Depot for Decky\nIcon=system-software-install\n'
            'Terminal=true\n'
            f'Exec=/usr/bin/python3 -I -c "{command}"\n'
            f'\n# Package: {url}\n# SHA-256: {digest}\n# Bootstrap source\n{commentary}\n')


def build(binary, out, base_url, development):
    source = Path(__file__).parent
    version = json.loads((source / "package.json").read_text())["version"]
    paths = {
        "plugin.json": source / "plugin.json",
        "package.json": source / "package.json",
        "main.py": source / "main.py",
        "dist/index.js": source / "dist/index.js",
        "bin/app-depot": binary,
        "LICENSE": source.parent / "LICENSE",
    }
    out.mkdir(parents=True, exist_ok=True)
    package = out / f"AppDepot-Decky-{version}-linux-x86_64.zip"
    with ZipFile(package, "w", compresslevel=9) as archive:
        for name, real in sorted(paths.items()):
            item = ZipInfo(f"AppDepot/{name}", (2000, 1, 1, 0, 0, 0))
            item.create_system = 3
            item.compress_type = ZIP_DEFLATED
            item.external_attr = (stat.S_IFREG | (0o755 if name.startswith("bin/") else 0o644)) << 16
            archive.writestr(item, real.read_bytes(), compresslevel=9)
    digest = hashlib.sha256(package.read_bytes()).hexdigest()
    (out / "SHA256SUMS").write_text(f"{digest}  {package.name}\n")
    desktop = out / "AppDepot-Decky-Installer.desktop"
    desktop.write_text(desktop_entry((source / "installer.py").read_text(),
                                    f"{base_url.rstrip('/')}/{package.name}", digest, version, development))
    desktop.chmod(0o755)
    print(package)
    print(desktop)
    print(digest)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--base-url", default="http://127.0.0.1:8765")
    parser.add_argument("--development-loopback", action="store_true")
    args = parser.parse_args()
    build(args.binary, args.out, args.base_url, args.development_loopback)
