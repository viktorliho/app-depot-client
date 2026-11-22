"""Signed Decky activation inputs. Development fixtures do not enroll a key."""

import fcntl
import hashlib
import json
import os
import re
import socket
import stat
import struct
import subprocess
import time
from contextlib import ExitStack
from pathlib import Path

from installer import EXPECTED, MAX_PACKAGE

REPOSITORY = "viktorliho/app-depot-client"
MAX_MANIFEST = 8192
SUBMISSION_HEADER = struct.Struct('!8sIIQ')
VERSION = re.compile(r"(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})-beta\.(0|[1-9][0-9]{0,8})")


def version_key(version):
    match = VERSION.fullmatch(version) if isinstance(version, str) else None
    if not match:
        raise ValueError("Invalid Decky update version")
    return tuple(map(int, match.groups()))


def manifest_bytes(version, payload):
    """Canonical encoding shared by the verifier and release signing tooling."""
    version_key(version)
    if not 0 < len(payload) <= MAX_PACKAGE:
        raise ValueError("Invalid Decky update size")
    return json.dumps({
        "architecture": "x86_64",
        "channel": "beta",
        "compatibility": "steamos-decky-plugin-api-1",
        "filename": f"AppDepot-Decky-v{version}-x86_64.zip",
        "layout": sorted(EXPECTED),
        "plugin": "AppDepot",
        "repository": REPOSITORY,
        "schema": 1,
        "size": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
        "tag": f"decky-v{version}",
        "version": version,
    }, sort_keys=True, separators=(",", ":")).encode("ascii")


def sealed_file(stack, data):
    fd = os.memfd_create("app-depot-update", os.MFD_ALLOW_SEALING | os.MFD_CLOEXEC)
    stack.callback(os.close, fd)
    with os.fdopen(os.dup(fd), "wb") as stream:
        stream.write(data)
    fcntl.fcntl(fd, fcntl.F_ADD_SEALS,
                fcntl.F_SEAL_WRITE | fcntl.F_SEAL_GROW | fcntl.F_SEAL_SHRINK | fcntl.F_SEAL_SEAL)
    return fd


def verify_release(manifest, signature, payload, trusted_public_key, installed_version):
    """Authenticate exact manifest bytes and compare them with the actual ZIP."""
    if (not 0 < len(manifest) <= MAX_MANIFEST or len(signature) != 64
            or not 0 < len(payload) <= MAX_PACKAGE or not 0 < len(trusted_public_key) <= 256):
        raise ValueError("Invalid signed update input size")
    # DER SubjectPublicKeyInfo prefix for a raw, 32-byte Ed25519 public key.
    if len(trusted_public_key) != 44 or not trusted_public_key.startswith(bytes.fromhex("302a300506032b6570032100")):
        raise ValueError("Expected a trusted Ed25519 public key")
    with ExitStack() as stack:
        key_fd = sealed_file(stack, trusted_public_key)
        manifest_fd = sealed_file(stack, manifest)
        signature_fd = sealed_file(stack, signature)
        result = subprocess.run(
            ["/usr/bin/openssl", "pkeyutl", "-verify", "-rawin", "-pubin", "-keyform", "DER",
             "-inkey", f"/proc/self/fd/{key_fd}", "-in", f"/proc/self/fd/{manifest_fd}",
             "-sigfile", f"/proc/self/fd/{signature_fd}"],
            env={"LANG": "C", "OPENSSL_CONF": "/dev/null"}, stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5,
            pass_fds=(key_fd, manifest_fd, signature_fd), check=False,
        )
    if result.returncode != 0:
        raise ValueError("Invalid Decky update signature")
    metadata = json.loads(manifest)
    if not isinstance(metadata, dict):
        raise ValueError("Invalid signed Decky manifest")
    version = metadata.get("version")
    if manifest != manifest_bytes(version, payload):
        raise ValueError("Signed Decky manifest does not match the candidate package")
    if version_key(version) <= version_key(installed_version):
        raise ValueError("Decky activation requires a newer beta version")
    return metadata


def authenticate_peer(connection, enrolled_uid):
    """Kernel-backed local account authentication, independent of protocol input."""
    if connection.family != socket.AF_UNIX or enrolled_uid <= 0:
        raise PermissionError("Invalid activation peer or enrollment")
    size = struct.calcsize("3i")
    _, uid, _ = struct.unpack("3i", connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, size))
    if uid != enrolled_uid:
        raise PermissionError("Update caller is not enrolled")


def receive_submission(connection, enrolled_uid, *, timeout=30):
    """Read one bounded fixture submission from a verified local peer."""
    authenticate_peer(connection, enrolled_uid)
    deadline = time.monotonic() + timeout

    def read(count, *, eof_ok=False):
        result = bytearray()
        while len(result) < count:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("Update IPC deadline expired")
            connection.settimeout(remaining)
            chunk = connection.recv(min(count - len(result), 64 * 1024))
            if not chunk:
                if eof_ok and not result:
                    break
                raise ValueError("Truncated update IPC request")
            result.extend(chunk)
        return bytes(result)

    magic, manifest_size, signature_size, package_size = SUBMISSION_HEADER.unpack(read(SUBMISSION_HEADER.size))
    if (magic != b"ADDECKY1" or not 0 < manifest_size <= MAX_MANIFEST or signature_size != 64
            or not 0 < package_size <= MAX_PACKAGE):
        raise ValueError("Invalid update IPC header")
    submission = (read(manifest_size), read(signature_size), read(package_size))
    if read(1, eof_ok=True):
        raise ValueError("Unexpected trailing update IPC data")
    return submission


def open_protected_directory(path):
    """Open a pinned directory only through root-owned, non-writable ancestors."""
    path = Path(path)
    if not path.is_absolute() or path.anchor != "/" or ".." in path.parts:
        raise ValueError("Expected a trusted absolute directory")
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    fd = os.open("/", flags)
    try:
        for name in (*path.parts[1:], None):
            info = os.fstat(fd)
            if info.st_uid != 0 or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
                raise PermissionError("Update directory has an untrusted ancestor")
            if name is not None:
                child = os.open(name, flags, dir_fd=fd)
                os.close(fd)
                fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise
