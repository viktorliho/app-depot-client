"""Development-only activation transaction for isolated root-owned fixtures.

The callbacks are test adapters. There is no Loader connection, installed
service, or selectable production destination.
"""

import fcntl
import json
import os
import re
import shutil
import stat
import tempfile
from contextlib import contextmanager
from pathlib import Path

from installer import PACKAGE, exchange, unpack
from update_security import open_protected_directory, verify_release


class ActivationFailed(RuntimeError):
    pass


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def generation(path):
    try:
        fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return None
    try:
        info = os.fstat(fd)
        if info.st_uid != 0 or info.st_mode & 0o022:
            raise PermissionError("Untrusted installed generation")
        return info.st_ino
    finally:
        os.close(fd)


@contextmanager
def fixture(path):
    path = Path(path)
    if os.geteuid() != 0 or path.parent != Path("/run") or not re.fullmatch(
        r"app-depot-update-fixture-[a-z0-9_]+", path.name,
    ):
        raise PermissionError("Activation fixture requires a disposable root-owned /run directory")
    fd = open_protected_directory(path)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        yield Path(f"/proc/self/fd/{fd}")
    finally:
        os.close(fd)


@contextmanager
def session_lease(root, *, exclusive):
    fd = os.open(root / "sessions.lock", os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC)
    try:
        info = os.fstat(fd)
        if info.st_uid != 0 or not stat.S_ISREG(info.st_mode) or info.st_mode & 0o022:
            raise PermissionError("Untrusted activation lease")
        fcntl.flock(fd, (fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH) | fcntl.LOCK_NB)
        yield
    finally:
        os.close(fd)


def journal(root, state):
    with tempfile.NamedTemporaryFile(dir=root, prefix=".journal-", delete=False) as file:
        temporary = Path(file.name)
        try:
            file.write(json.dumps(state, separators=(",", ":")).encode("ascii"))
            file.flush()
            os.fsync(file.fileno())
            temporary.replace(root / "journal.json")
            os.fsync(int(root.name))
        finally:
            temporary.unlink(missing_ok=True)


def read_record(path):
    fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(fd, "rb") as file:
        info = os.fstat(file.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != 0 or info.st_mode & 0o022:
            raise PermissionError("Untrusted fixture metadata")
        content = file.read(8193)
    if len(content) > 8192:
        raise ValueError("Unexpected fixture metadata size")
    return json.loads(content)


def installed_version(path):
    identity = read_record(path / "plugin.json")
    if identity != {"api_version": 1, "name": "App Depot", "author": "App Depot", "flags": []}:
        raise ValueError("Unexpected installed plugin identity")
    metadata = read_record(path / "package.json")
    if metadata.get("name") != "app-depot-decky":
        raise ValueError("Unexpected installed package identity")
    return metadata["version"]


def finish(root, pending):
    if generation(pending) is not None:
        shutil.rmtree(pending)
        os.fsync(int(root.name))
    (root / "journal.json").unlink(missing_ok=True)
    os.fsync(int(root.name))


def recover_locked(root, reload_previous):
    pending = root / ".candidate"
    current = root / PACKAGE
    recorded = root / "journal.json"
    try:
        state = read_record(recorded)
    except FileNotFoundError:
        # An unjournaled candidate was never eligible for an exchange.
        finish(root, pending)
        return
    present = (generation(current), generation(pending))
    old, new = state["old"], state["new"]
    if state["phase"] == "healthy" and present in ((new, old), (new, None)):
        finish(root, pending)
    elif state["phase"] == "prepared" and present in ((old, new), (new, old), (old, None)):
        if present == (new, old):
            exchange(current, pending)
            os.fsync(int(root.name))
        # A prior process can have restored the tree but died before reload.
        if present != (old, None):
            try:
                reload_previous()
            except Exception as error:
                raise ActivationFailed("Previous generation restored; manual reload required") from error
        finish(root, pending)
    else:
        raise ActivationFailed("Unrecognized generation pair; manual recovery required")


def recover(path, reload_previous):
    with fixture(path) as root, session_lease(root, exclusive=True):
        recover_locked(root, reload_previous)


def activate(path, manifest, signature, payload, key, reload_candidate, reload_previous,
             *, checkpoint=lambda _phase: None):
    with fixture(path) as root, session_lease(root, exclusive=True):
        recover_locked(root, reload_previous)
        current = root / PACKAGE
        old = generation(current)
        if old is None:
            raise ValueError("An installed App Depot fixture is required")
        metadata = verify_release(manifest, signature, payload, key, installed_version(current))
        pending = root / ".candidate"
        pending.mkdir(mode=0o700)
        try:
            unpack(payload, pending, metadata["version"])
            pending.chmod(0o755)
            for child in sorted((p for p in pending.rglob("*") if p.is_dir()),
                                key=lambda p: len(p.parts), reverse=True):
                sync_dir(child)
            sync_dir(pending)
            os.fsync(int(root.name))
            state = {"phase": "prepared", "old": old, "new": generation(pending)}
            journal(root, state)
            checkpoint("prepared")
            exchange(current, pending)
            os.fsync(int(root.name))
            checkpoint("exchanged")
            expected = (metadata["version"], metadata["sha256"], state["new"])
            if reload_candidate(expected) != expected:
                raise ActivationFailed("The candidate generation did not acknowledge health")
            journal(root, {**state, "phase": "healthy"})
            checkpoint("healthy")
            finish(root, pending)
        except Exception:
            recover_locked(root, reload_previous)
            raise
