#!/usr/bin/env python3
"""Build a local Decky review bundle with a deliberately public fixture key."""

import argparse
import hashlib
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "decky"))
from package import build  # noqa: E402
from test_update_security import TEST_PUBLIC, test_sign  # noqa: E402
from update_security import manifest_bytes  # noqa: E402


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    version = json.loads((ROOT / "decky/package.json").read_text())["version"]
    out = args.out / "DEVELOPMENT-ONLY"
    build(args.binary, out, "http://127.0.0.1:8765", True)
    package = out / f"AppDepot-Decky-{version}-linux-x86_64.zip"
    manifest = manifest_bytes(version, package.read_bytes())
    files = {
        "DEVELOPMENT-ONLY.manifest.json": manifest,
        "DEVELOPMENT-ONLY.manifest.sig": test_sign(manifest),
        "DEVELOPMENT-ONLY.public-key.der": TEST_PUBLIC,
        "README.txt": (
            "Development only. The manifest uses a publicly known test key.\n"
            "The Desktop installer reads the neighboring ZIP offline and does not enroll a helper.\n"
            "The manifest's filename field is the canonical logical name, not a published asset.\n"
            "Do not enroll this key or distribute this bundle as an official release.\n"
        ).encode("ascii"),
    }
    for name, data in files.items():
        (out / name).write_bytes(data)
    with (out / "SHA256SUMS").open("w") as hashes:
        for file in sorted(out.iterdir()):
            if file.name != "SHA256SUMS" and file.is_file():
                hashes.write(f"{hashlib.sha256(file.read_bytes()).hexdigest()}  {file.name}\n")


if __name__ == "__main__":
    main()
