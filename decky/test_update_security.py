import hashlib
import json
import os
import pwd
import signal
import socket
import subprocess
import tempfile
import unittest
from pathlib import Path

from installer import unpack
from test_decky import archive
from update_security import (SUBMISSION_HEADER, authenticate_peer, manifest_bytes,
                             open_protected_directory, receive_submission, verify_release)
from update_transaction_fixture import ActivationFailed, activate, recover, session_lease

# RFC 8032 public test vector. This key is public and must never be enrolled.
TEST_SEED = "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"
TEST_PUBLIC = bytes.fromhex("302a300506032b6570032100d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a")
OLD = "0.1.0-beta.6"
NEW = "0.1.0-beta.7"


def test_sign(data):
    seed = bytes.fromhex(TEST_SEED)
    key = bytes.fromhex("302e020100300506032b657004220420") + seed
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        (root / "key.der").write_bytes(key)
        (root / "manifest").write_bytes(data)
        return subprocess.run(
            ["/usr/bin/openssl", "pkeyutl", "-sign", "-rawin", "-keyform", "DER",
             "-inkey", "key.der", "-in", "manifest"], cwd=root, check=True,
            capture_output=True,
        ).stdout


class SignedUpdateTests(unittest.TestCase):
    def test_verify_and_reject_modified_package_signature_and_downgrade(self):
        package = archive(NEW)
        manifest = manifest_bytes(NEW, package)
        signature = test_sign(manifest)
        self.assertEqual(verify_release(manifest, signature, package, TEST_PUBLIC, OLD)["version"], NEW)
        for changed_manifest, changed_signature, changed_package, installed in (
            (manifest + b" ", signature, package, OLD),
            (manifest, bytes(64), package, OLD),
            (manifest, signature[:-1], package, OLD),
            (manifest, signature, package[:-1], OLD),
            (manifest, signature, package[:-1] + b"x", OLD),
            (manifest, signature, package, NEW),
        ):
            with self.subTest(installed=installed), self.assertRaises(ValueError):
                verify_release(changed_manifest, changed_signature, changed_package, TEST_PUBLIC, installed)
        for field, value in (
            ("plugin", "Other"), ("channel", "stable"), ("size", 1), ("schema", True),
            ("repository", "example/other"), ("tag", "v0.1.3"), ("layout", []),
            ("filename", "foreign.zip"), ("architecture", "aarch64"), ("compatibility", "other"),
        ):
            changed = json.loads(manifest)
            changed[field] = value
            raw = json.dumps(changed, sort_keys=True, separators=(",", ":")).encode("ascii")
            with self.subTest(field=field), self.assertRaises(ValueError):
                verify_release(raw, test_sign(raw), package, TEST_PUBLIC, OLD)

    def test_kernel_peer_credentials_and_untrusted_path(self):
        one, two = socket.socketpair()
        with one, two:
            if os.getuid() != 0:
                authenticate_peer(one, os.getuid())
            with self.assertRaises(PermissionError):
                authenticate_peer(one, os.getuid() + 1)
            with self.assertRaises(PermissionError):
                authenticate_peer(one, 0)
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaises(PermissionError):
                open_protected_directory(Path(temp))
        with self.assertRaises(ValueError):
            open_protected_directory(Path("/run/../run"))


    @unittest.skipIf(os.getuid() == 0, "Root is never an enrolled account")
    def test_ipc_rejects_bad_header_truncation_extra_bytes_and_stall(self):
        header = SUBMISSION_HEADER.pack(b"ADDECKY1", 1, 64, 1)
        for message in (
            SUBMISSION_HEADER.pack(b"ADDECKY1", 8193, 64, 1),
            SUBMISSION_HEADER.pack(b"BADMAGIC", 1, 64, 1),
            header + b"x",
            header + bytes(67),
        ):
            with self.subTest(message_size=len(message)):
                a, b = socket.socketpair()
                with a, b:
                    b.sendall(message)
                    b.shutdown(socket.SHUT_WR)
                    with self.assertRaises(ValueError):
                        receive_submission(a, os.getuid())
        a, b = socket.socketpair()
        with a, b, self.assertRaises(TimeoutError):
            receive_submission(a, os.getuid(), timeout=0.02)


class SimulatedPowerLoss(BaseException):
    pass


@unittest.skipUnless(os.geteuid() == 0, "Requires disposable privileged fixture under /run")
class RootActivationTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix="app-depot-update-fixture-", dir="/run")
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        lease = self.root / "sessions.lock"
        lease.touch()
        lease.chmod(0o644)
        self.current = self.root / "AppDepot"
        self.current.mkdir()
        unpack(archive(OLD), self.current, OLD)
        self.user_data = self.root / "bindings-fixture"
        self.user_data.write_text("steam-id-fixture")
        self.package = archive(NEW)
        self.manifest = manifest_bytes(NEW, self.package)
        self.signature = test_sign(self.manifest)
        self.reloads = []

    def version(self):
        return json.loads((self.current / "package.json").read_text())["version"]

    def restore(self):
        self.reloads.append(self.version())

    def activate(self, health, **options):
        activate(self.root, self.manifest, self.signature, self.package, TEST_PUBLIC,
                 health, self.restore, **options)

    def test_health_switch_and_failed_reload(self):
        with self.assertRaises(ActivationFailed):
            self.activate(lambda _expected: (OLD, "", None))
        self.assertEqual(self.version(), OLD)
        self.assertEqual(self.reloads, [OLD])
        def healthy(expected):
            version, digest, _generation = expected
            self.assertEqual(version, self.version())
            self.assertEqual(digest, hashlib.sha256(self.package).hexdigest())
            return expected
        self.activate(healthy)
        self.assertEqual(self.version(), NEW)
        self.assertEqual(self.user_data.read_text(), "steam-id-fixture")
        self.assertFalse((self.root / "journal.json").exists())

    def test_recover_after_three_durable_boundaries(self):
        for phase in ("prepared", "exchanged", "healthy"):
            with self.subTest(phase=phase):
                def interrupt(progress):
                    if progress == phase:
                        raise SimulatedPowerLoss()
                with self.assertRaises(SimulatedPowerLoss):
                    self.activate(lambda expected: expected, checkpoint=interrupt)
                recover(self.root, self.restore)
                recover(self.root, self.restore)
                self.assertEqual(self.version(), NEW if phase == "healthy" else OLD)
                self.assertFalse((self.root / ".candidate").exists())
                self.assertFalse((self.root / "journal.json").exists())

    def test_process_kill_restores_previous_and_retries_reload(self):
        child = os.fork()
        if child == 0:
            def stop(phase):
                if phase == "exchanged":
                    os.kill(os.getpid(), signal.SIGKILL)
            try:
                self.activate(lambda expected: expected, checkpoint=stop)
            finally:
                os._exit(1)
        self.assertEqual(os.waitstatus_to_exitcode(os.waitpid(child, 0)[1]), -signal.SIGKILL)
        self.assertEqual(self.version(), NEW)

        def failed_reload():
            raise OSError("fixture loader is unavailable")

        with self.assertRaisesRegex(ActivationFailed, "manual reload required"):
            recover(self.root, failed_reload)
        self.assertEqual(self.version(), OLD)
        self.assertTrue((self.root / "journal.json").exists())
        recover(self.root, self.restore)
        self.assertEqual(self.reloads, [OLD])
        self.assertFalse((self.root / "journal.json").exists())

    def test_live_uid_and_shared_session_lease(self):
        account = pwd.getpwnam("nobody")
        self.root.chmod(0o711)
        endpoint = self.root / "test.sock"
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listener:
            listener.settimeout(4)
            listener.bind(str(endpoint))
            os.chown(endpoint, account.pw_uid, account.pw_gid)
            endpoint.chmod(0o600)
            listener.listen(1)
            child = os.fork()
            if child == 0:
                try:
                    listener.close()
                    os.setgroups([])
                    os.setgid(account.pw_gid)
                    os.setuid(account.pw_uid)
                    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                        connection.connect(str(endpoint))
                        connection.sendall(SUBMISSION_HEADER.pack(
                            b"ADDECKY1", len(self.manifest), len(self.signature), len(self.package),
                        ) + self.manifest + self.signature + self.package)
                        connection.shutdown(socket.SHUT_WR)
                    os._exit(0)
                except BaseException:
                    os._exit(1)
            try:
                connection, _ = listener.accept()
                with connection:
                    self.assertEqual(receive_submission(connection, account.pw_uid),
                                     (self.manifest, self.signature, self.package))
                    with self.assertRaises(PermissionError):
                        authenticate_peer(connection, os.getuid())
            finally:
                self.assertEqual(os.waitpid(child, 0)[1], 0)
        with session_lease(self.root, exclusive=False):
            with session_lease(self.root, exclusive=False):
                with self.assertRaises(BlockingIOError):
                    self.activate(lambda expected: expected)
        self.assertEqual(self.version(), OLD)

    def test_unsafe_package_symlink_and_concurrent_transaction(self):
        unsafe = archive(NEW, "AppDepot/../foreign")
        manifest = manifest_bytes(NEW, unsafe)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            activate(self.root, manifest, test_sign(manifest), unsafe, TEST_PUBLIC,
                     lambda expected: expected, self.restore)
        with self.assertRaises(ValueError):
            activate(self.root, manifest_bytes(NEW, b"wrong"), bytes(64), b"wrong", TEST_PUBLIC,
                     lambda expected: expected, self.restore)
        corrupt = b"invalid zip"
        corrupt_manifest = manifest_bytes(NEW, corrupt)
        from zipfile import BadZipFile
        with self.assertRaises(BadZipFile):
            activate(self.root, corrupt_manifest, test_sign(corrupt_manifest), corrupt, TEST_PUBLIC,
                     lambda expected: expected, self.restore)
        alias = self.root / ".candidate"
        alias.symlink_to(self.current, target_is_directory=True)
        with self.assertRaises(OSError):
            self.activate(lambda expected: expected)
        alias.unlink()
        journal_alias = self.root / "journal.json"
        journal_alias.symlink_to(self.user_data)
        with self.assertRaises(OSError):
            self.activate(lambda expected: expected)
        journal_alias.unlink()
        def check(expected):
            with self.assertRaises(BlockingIOError):
                recover(self.root, self.restore)
            return expected
        self.activate(check)
        self.assertEqual(self.version(), NEW)
