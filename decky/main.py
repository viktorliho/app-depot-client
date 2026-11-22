"""Unprivileged Decky RPC boundary. All Server and library operations use Rust."""

import asyncio
import base64
import hashlib
import json
import logging
import os
import re
import ssl
from pathlib import Path
import stat
import struct
import tempfile
from urllib.error import HTTPError, URLError
from uuid import UUID


ROLES = {
    "steam.grid.vertical": "p", "steam.grid.horizontal": "",
    "steam.hero": "_hero", "logo": "_logo", "icon": "_appdepot_icon",
}


def identity(value):
    if str(UUID(value)) != value:
        raise ValueError("Invalid App Depot identity")
    return value


def account_id(value):
    if not isinstance(value, str) or not value.isascii() or not value.isdecimal():
        raise ValueError("Steam account unavailable")
    if not 0 < int(value) <= 0xFFFFFFFF or str(int(value)) != value:
        raise ValueError("Invalid Steam account")
    return value


def read_shortcuts(path):
    """Read a snapshot of Valve's binary shortcuts file without writing it."""
    if not path.exists():
        return []
    with path.open("rb") as source:
        data = source.read(16 * 1024 * 1024 + 1)
    if len(data) > 16 * 1024 * 1024:
        raise ValueError("Steam shortcut snapshot exceeds size limit")
    cursor = 0

    def take(count):
        nonlocal cursor
        value = data[cursor:cursor + count]
        if len(value) != count:
            raise ValueError("Incomplete Steam shortcut snapshot")
        cursor += count
        return value

    def string():
        end = data.find(b"\0", cursor)
        if end == -1:
            raise ValueError("Incomplete Steam shortcut string")
        return take(end - cursor + 1)[:-1].decode("utf-8")

    def fields(depth):
        if depth > 12:
            raise ValueError("Steam shortcut snapshot is too deeply nested")
        result = {}
        while (kind := take(1)[0]) != 8:
            key = string().lower()
            if key in result:
                raise ValueError("Duplicate Steam shortcut field")
            if kind == 0:
                value = fields(depth + 1)
            elif kind == 1:
                value = string()
            elif kind == 2:
                value = struct.unpack("<I", take(4))[0]
            elif kind in (3, 4, 6):
                value = take(4)
            elif kind in (7, 10):
                value = take(8)
            else:
                raise ValueError(f"Unsupported Steam shortcut field type {kind}")
            result[key] = value
        return result

    shortcuts = fields(0)["shortcuts"]
    if cursor != len(data):
        raise ValueError("Unexpected trailing Steam shortcut data")
    rows = list(shortcuts.values())
    ids = [row["appid"] for row in rows]
    if len(ids) != len(set(ids)):
        raise ValueError("Duplicate shortcut AppID in Steam")
    return rows


def atomic(path, data, mode=0o600):
    with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
        tmp = Path(output.name)
        try:
            os.fchmod(output.fileno(), mode)
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
            tmp.replace(path)
            directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        finally:
            tmp.unlink(missing_ok=True)


class RpcFailure(RuntimeError):
    def __init__(self, code):
        super().__init__(code)
        self.code = code


class Plugin:
    async def _main(self):
        if os.geteuid() == 0:
            raise RuntimeError("App Depot requires an unprivileged Decky backend")
        self.home = Path(os.environ.get("DECKY_USER_HOME", str(Path.home())))
        self.plugin = Path(os.environ.get("DECKY_PLUGIN_DIR", str(Path(__file__).parent)))
        self.data = self.home / ".local/share/app-depot-decky"
        self.data.mkdir(mode=0o700, parents=True, exist_ok=True)
        if self.data.is_symlink() or self.data.stat().st_uid != os.getuid():
            raise RuntimeError("Unsafe App Depot data directory")
        self.ledger = self.data / "bindings.json"
        self.lock = asyncio.Lock()
        self.children = set()
        self.closing = False
        self.update_lock = asyncio.Lock()
        self.update_candidate = None
        self.release_pages = None

    async def _unload(self):
        self.closing = True
        children = list(self.children)
        for child in children:
            if child.returncode is None:
                child.kill()
        await asyncio.gather(*(child.wait() for child in children))

    async def _rust(self, *args, token="", allow_http=False, image=None):
        if self.closing:
            raise RuntimeError("App Depot is reloading")
        # Decky dispatches by method name, including underscore methods.
        if type(allow_http) is not bool:
            raise ValueError("HTTP consent must be explicit")
        if allow_http and not (args[:2] == ("decky", "test")
                               or args[:3] == ("--json", "profile", "add")):
            raise ValueError("HTTP consent only applies to Server setup")
        match args:
            case ("decky", "status" | "discover" | "installed"):
                pass
            case ("decky", "uninstall", profile, app, revision):
                identity(profile)
                identity(app)
                if not isinstance(revision, str) or len(revision) != 64 or any(ch not in "0123456789abcdef" for ch in revision):
                    raise ValueError("Invalid local selection revision")
            case ("decky", "mute-grid"):
                if type(image) is not bytes or len(image) > 16 * 1024 * 1024:
                    raise ValueError("Invalid cached grid")
            case ("decky", "test", origin) | ("--json", "profile", "add", "App Depot Server", origin):
                self._origin(origin)
            case ("decky", "sync" | "catalog", profile) | ("profile", "token-remove", profile):
                identity(profile)
            case ("profile", "token-set", profile, "--token-stdin"):
                identity(profile)
            case ("decky", "artwork", profile, app, role, "muted" | "color", known) if role in ROLES:
                identity(profile)
                identity(app)
                if not isinstance(known, str) or len(known) > 96 or known.startswith("-"):
                    raise ValueError("Invalid artwork revision")
            case _:
                raise ValueError("Unsupported App Depot RPC command")
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("APP_DEPOT_")}
        env["HOME"] = str(self.home)
        env["APP_DEPOT_DECKY_RPC"] = "1"
        runtime = Path("/run/user") / str(os.getuid())
        if (runtime / "bus").is_socket():
            env.setdefault("XDG_RUNTIME_DIR", str(runtime))
            env.setdefault("DBUS_SESSION_BUS_ADDRESS", f"unix:path={runtime / 'bus'}")
        if not isinstance(token, str) or len(token.encode()) > 16384 or any(c in token for c in ("\r", "\n", "\0")):
            raise RpcFailure("invalid_token")
        flags = ["--allow-insecure-http"] if allow_http else []
        child = await asyncio.create_subprocess_exec(
            str(self.plugin / "bin/app-depot"), *flags, *args, stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE, env=env,
        )
        self.children.add(child)
        try:
            if self.closing:
                raise RuntimeError("App Depot is reloading")
            output, error = await asyncio.wait_for(child.communicate(image if args == ("decky", "mute-grid") else token.encode()), 180)
            if child.returncode:
                try:
                    code = json.loads(error)["code"]
                except (ValueError, KeyError, TypeError):
                    code = "internal"
                raise RpcFailure(code)
            if args[:2] in (("profile", "token-set"), ("profile", "token-remove")):
                return None
            return json.loads(output) if output else None
        finally:
            if child.returncode is None:
                child.kill()
                await child.wait()
            self.children.discard(child)

    async def request(self, method, *args):
        operations = {
            "status": self.status, "discover": self.discover, "test": self.test,
            "configure": self.configure, "catalog": self.catalog, "artwork": self.artwork,
            "shortcut": self.shortcut, "acknowledge": self.acknowledge, "binding": self.binding,
            "check_update": self.check_update, "verify_update": self.verify_update,
            "installed": self.installed, "uninstall": self.uninstall,
            "grid_states": self.grid_states, "finish_grids": self.finish_grids,
        }
        if not isinstance(method, str) or method not in operations:
            return {"ok": False, "code": "internal"}
        try:
            return {"ok": True, "value": await operations[method](*args)}
        except RpcFailure as error:
            if method in {"check_update", "verify_update", "shortcut", "artwork", "uninstall"}:
                logging.getLogger(__name__).info("App Depot RPC %s: %s", method, error.code)
            return {"ok": False, "code": error.code}
        except asyncio.TimeoutError:
            return {"ok": False, "code": "timeout"}
        except Exception as error:
            # Details can contain Server response text or user paths. Record only
            # the action and exception class for local diagnostics.
            logging.getLogger(__name__).warning("App Depot RPC %s: %s",
                                                method,
                                                type(error).__name__)
            return {"ok": False, "code": "internal"}

    def package_version(self):
        package = json.loads((self.plugin / "package.json").read_text())
        version = package.get("version")
        if package.get("name") != "app-depot-decky" or not isinstance(version, str) or not re.fullmatch(
            r"(?:0|[1-9][0-9]{0,8})\.(?:0|[1-9][0-9]{0,8})\.(?:0|[1-9][0-9]{0,8})-beta\.(?:0|[1-9][0-9]{0,8})",
            version,
        ):
            raise RpcFailure("package_metadata_invalid")
        return version

    async def check_update(self):
        from decky_updates import ReleasePages, find_update

        if self.update_lock.locked():
            raise RpcFailure("update_busy")
        async with self.update_lock:
            self.update_candidate = None
            if self.release_pages is None:
                self.release_pages = ReleasePages()
            try:
                self.update_candidate = await asyncio.to_thread(find_update, self.package_version(), self.release_pages)
            except HTTPError as error:
                if error.code == 429 or (error.code == 403 and (
                    error.headers.get("X-RateLimit-Remaining") == "0" or error.headers.get("Retry-After")
                )):
                    raise RpcFailure("update_rate_limited") from None
                raise RpcFailure("update_http_failed") from None
            except URLError as error:
                code = "update_tls_failed" if isinstance(error.reason, ssl.SSLError) else "update_network_failed"
                raise RpcFailure(code) from None
            except ssl.SSLError:
                raise RpcFailure("update_tls_failed") from None
            except OSError:
                raise RpcFailure("update_network_failed") from None
            except (ValueError, TypeError):
                raise RpcFailure("update_metadata_invalid") from None
            if self.update_candidate:
                return {key: self.update_candidate[key] for key in ("version", "notes", "url")}
            return None

    async def verify_update(self):
        from decky_updates import verify_update

        if self.update_lock.locked():
            raise RpcFailure("update_busy")
        async with self.update_lock:
            if not self.update_candidate:
                raise RpcFailure("update_check_failed")
            try:
                await asyncio.to_thread(verify_update, self.update_candidate)
            except Exception:
                # Network URLs and archive paths remain inside this boundary.
                raise RpcFailure("update_verification_failed") from None

    async def discover(self):
        return await self._rust("decky", "discover")

    async def status(self):
        result = await self._rust("decky", "status")
        result["coreVersion"] = result["version"]
        result["version"] = self.package_version()
        return result

    async def test(self, origin, token, allow_http=False):
        self._origin(origin)
        return await self._rust("decky", "test", origin, token=token, allow_http=allow_http)

    @staticmethod
    def _origin(origin):
        if not isinstance(origin, str) or len(origin) > 2048 or origin.startswith("-"):
            raise RpcFailure("invalid_url")

    async def configure(self, origin, token, allow_http=False):
        origin = (await self.test(origin, token, allow_http))["origin"]
        async with self.lock:
            profiles = (await self.status())["profiles"]
            profile = next((p for p in profiles if p["origin"].rstrip("/") == origin.rstrip("/")), None)
            if profile is None:
                profile = await self._rust("--json", "profile", "add", "App Depot Server", origin,
                                           allow_http=allow_http)
            ident = identity(profile["id"])
            if token:
                await self._rust("profile", "token-set", ident, "--token-stdin", token=token)
            elif profile.get("authenticated"):
                raise RpcFailure("saved_credentials")
            return ident

    async def catalog(self, profile, refresh):
        return await self._rust("decky", "sync" if refresh else "catalog", identity(profile))

    async def installed(self):
        return await self._rust("decky", "installed")

    async def uninstall(self, profile, app, revision):
        profile, app = identity(profile), identity(app)
        # Record before starting so interruption can never lose the artwork intent.
        async with self.lock:
            ledger = self._load()
            for key, binding in ledger.items():
                if key.split("/")[1:] == [profile, app]:
                    binding["pendingGrids"] = True
            self._save(ledger)
        return await self._rust("decky", "uninstall", profile, app, revision)

    async def grid_states(self, account):
        prefix = account_id(account) + "/"
        async with self.lock:
            result = []
            for key, binding in self._load().items():
                if not key.startswith(prefix):
                    continue
                profile, app = key[len(prefix):].split("/")
                modes = []
                for role in ("steam.grid.vertical", "steam.grid.horizontal"):
                    state = binding["artwork"].get(role, {})
                    revision = state.get("pendingRevision", state.get("revision", ""))
                    if revision.endswith((":true:png1", ":false:png1")):
                        modes.append(revision.endswith(":false:png1"))
                result.append({"profile": profile, "app": app, "modes": modes,
                               "pending": bool(binding.get("pendingGrids"))})
            return result

    async def finish_grids(self, account, profile, app):
        async with self.lock:
            ledger, _, binding = self._binding(account, profile, app)
            if binding and binding.pop("pendingGrids", False):
                self._save(ledger)

    def _config(self, account):
        return self.home / ".steam/root/userdata" / account_id(account) / "config"

    def _load(self):
        if not self.ledger.exists():
            return {}
        return json.loads(self.ledger.read_bytes())

    def _save(self, ledger):
        atomic(self.ledger, json.dumps(ledger, sort_keys=True).encode())

    def _binding(self, account, profile, app):
        key = "/".join((account_id(account), identity(profile), identity(app)))
        ledger = self._load()
        return ledger, key, ledger.get(key)

    def _owned(self, account, binding, observation=None, require_disk=True):
        if binding is None:
            return None
        if observation is not None:
            if (type(observation.get("appid")) is not int
                    or observation["appid"] != binding.get("appid")
                    or observation.get("launchoptions") != binding["options"]
                    or observation.get("exe", "").strip('"') != binding["target"]
                    or not isinstance(observation.get("appname"), str)):
                raise RpcFailure("shortcut_ownership")
            return observation
        rows = read_shortcuts(self._config(account) / "shortcuts.vdf")
        matches = [row for row in rows
                   if row.get("launchoptions") == binding["options"]
                   and row.get("exe", "").strip('"') == binding["target"]]
        if len(matches) > 1:
            raise RpcFailure("shortcut_ambiguous")
        actual = matches[0] if matches else None
        expected = binding.get("appid")
        if expected is not None and (
            actual is not None and actual["appid"] != expected
            or actual is None and (
                (require_disk and binding.get("confirmed", False))
                or any(row["appid"] == expected for row in rows)
            )
        ):
            raise RpcFailure("shortcut_ownership")
        return actual

    def _setup_shortcut(self, account, profile, app, binding, row):
        """Return an in-place repair only for acknowledged, unconfirmed entries."""
        appid = binding.get("appid")
        if binding.get("confirmed") or not appid or row.get("appid") != appid:
            return None
        alias = self.data / "launchers" / profile / app / "play"
        if (binding["target"] != str(alias) or not alias.is_symlink()
                or alias.readlink() != self.plugin / "bin/app-depot"):
            return None
        expected = {
            "exe": f'"{alias}"', "startdir": f'"{alias.parent}"',
            "launchoptions": binding["options"], "appname": binding["name"],
        }
        before = binding.get("repair")
        values = {field: row.get(field) for field in expected}
        if before is None:
            auto_start = f'"{alias.parent}/'
            legacy = dict(exe=f'""{alias}""', startdir=auto_start, launchoptions="", appname='play"')
            if values == legacy:
                snapshots = read_shortcuts(self._config(account) / "shortcuts.vdf")
                snapshot = next((record for record in snapshots if record["appid"] == appid), {})
                if any(snapshot.get(field) != value for field, value in legacy.items()):
                    return None
            elif not (
                binding.get("initializing") and values["exe"] in (str(alias), expected["exe"])
                and values["launchoptions"] in ("", expected["launchoptions"])
                and values["appname"] in ("play", expected["appname"])
                and values["startdir"] in (auto_start, auto_start + '"', str(alias.parent) + "/", str(alias.parent), expected["startdir"])
            ):
                return None
            before = values
        if any(values[field] not in (before[field], desired) for field, desired in expected.items()):
            raise RpcFailure("shortcut_changed")
        if values == expected:
            binding.pop("repair", None)
            binding.pop("initializing", None)
            return {"action": "keep", "appid": appid}
        binding["repair"] = before
        return {"action": "setup", "appid": appid, "fields": {
            field: value for field, value in expected.items() if values[field] != value
        }}

    async def shortcut(self, account, profile, app, name, observation=None):
        if not isinstance(name, str) or not name.strip() or len(name) > 512 or "\0" in name:
            raise ValueError("Invalid shortcut display name")
        async with self.lock:
            ledger, key, binding = self._binding(account, profile, app)
            if binding is not None:
                if observation is None and binding.get("appid") is not None:
                    return {"action": "inspect", "appid": binding["appid"]}
                if observation is not None:
                    setup = self._setup_shortcut(account, profile, app, binding, observation)
                    if setup is not None:
                        if setup["action"] == "keep":
                            binding["confirmed"] = True
                        self._save(ledger)
                        return setup
                    if binding.get("repair") or binding.get("initializing"):
                        raise RpcFailure("shortcut_unconfirmed")
                actual = self._owned(account, binding, observation)
                if actual is None:
                    return {"action": "pending"}
                if not actual["appid"] & 0x80000000:
                    raise RpcFailure("shortcut_identity")
                rename = binding.get("rename")
                if rename and actual.get("appname") == rename:
                    binding["name"] = binding.pop("rename")
                if not binding.get("confirmed"):
                    binding["appid"] = actual["appid"]
                    binding["confirmed"] = True
                    self._save(ledger)
                elif rename and binding["name"] == rename:
                    self._save(ledger)
                if actual.get("appname") == binding["name"] and name != binding["name"]:
                    binding["rename"] = name
                    self._save(ledger)
                    return {"action": "rename", "appid": actual["appid"], "name": name}
                return {"action": "keep", "appid": actual["appid"]}
            if not self._config(account).is_dir():
                raise RpcFailure("shortcut_storage")
            alias = self.data / "launchers" / profile / app / "play"
            alias.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            if alias.is_symlink():
                if alias.readlink() != self.plugin / "bin/app-depot":
                    raise RpcFailure("shortcut_launcher")
            else:
                alias.symlink_to(self.plugin / "bin/app-depot")
            target = str(alias)
            if any(character in target for character in ('"', "\n", "\r", "%", "$", "`", "\\")):
                raise RpcFailure("shortcut_launcher")
            binding = {"name": name, "target": target, "options": f"decky play {profile} {app}",
                       "artwork": {}, "initializing": True}
            if self._owned(account, binding) is not None:
                raise RpcFailure("shortcut_ambiguous")
            ledger[key] = binding
            self._save(ledger)
            return {"action": "create", "target": target, "options": binding["options"]}

    async def binding(self, account, profile, app):
        async with self.lock:
            _, _, binding = self._binding(account, profile, app)
            return binding["appid"] if binding and binding.get("confirmed") else None

    async def acknowledge(self, account, profile, app, appid):
        if type(appid) is not int or not 0x80000000 <= appid <= 0xFFFFFFFF:
            raise ValueError("Invalid non-Steam AppID")
        async with self.lock:
            ledger, _, binding = self._binding(account, profile, app)
            if binding is None or binding.get("confirmed") or binding.get("appid") not in (None, appid):
                raise RpcFailure("shortcut_identity")
            binding["appid"] = appid
            self._save(ledger)

    def _grid(self, account, appid, role):
        grid = self._config(account) / "grid"
        base = f"{appid}{ROLES[role]}"
        paths = [grid / f"{base}.{ext}" for ext in ("png", "jpg", "jpeg", "webp")]
        return paths

    @staticmethod
    def _hash(path):
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as stream:
            if os.fstat(fd).st_size > 16 * 1024 * 1024:
                raise ValueError("Steam artwork exceeds size limit")
            return hashlib.file_digest(stream, "sha256").hexdigest()

    async def artwork(self, account, profile, app, role, downloaded, observation=None):
        if role not in ROLES:
            raise ValueError("Unknown artwork role")
        async with self.lock:
            ledger, _, binding = self._binding(account, profile, app)
            row = self._owned(account, binding, observation)
            if not row or binding.get("appid") is None:
                return None
            appid = row["appid"]
            state = binding["artwork"].get(role, {})
            files = [path for path in self._grid(account, appid, role) if path.is_file()]
            digest = self._hash(files[0]) if len(files) == 1 else None
            if len(files) > 1 or (files and digest not in (state.get("digest"), state.get("pendingDigest"))):
                return None
            if role == "icon" and observation is not None:
                disk = self._owned(account, binding, require_disk=False)
                if disk is None:
                    return None
                row = disk
            if role == "icon" and row.get("icon") not in (None, "", state.get("path"), state.get("pendingPath")):
                return None
            if digest and digest == state.get("pendingDigest"):
                state["digest"] = digest
                if role != "icon" or row.get("icon") == state.get("pendingPath"):
                    state["revision"] = state.pop("pendingRevision")
                    state.pop("pendingDigest")
                    if role == "icon":
                        state["path"] = state.pop("pendingPath")
                    self._save(ledger)
            known = state.get("revision", "") if files else ""
            if (binding.get("pendingGrids") and not downloaded
                    and role in ("steam.grid.vertical", "steam.grid.horizontal")):
                if known.endswith(":true:png1"):
                    return None
                if known.endswith(":false:png1") and digest:
                    # Read the exact previously acknowledged Steam PNG, with no symlink following.
                    fd = os.open(files[0], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
                    with os.fdopen(fd, "rb") as image:
                        if not stat.S_ISREG(os.fstat(image.fileno()).st_mode):
                            raise ValueError("Invalid cached grid")
                        source = image.read(16 * 1024 * 1024 + 1)
                    if hashlib.sha256(source).hexdigest() != digest:
                        raise ValueError("Cached grid changed")
                    art = await self._rust("decky", "mute-grid", image=source)
                    art["revision"] = known.removesuffix(":false:png1") + ":true:png1"
                else:
                    art = await self._rust("decky", "artwork", profile, app, role, "muted", known)
            else:
                art = await self._rust("decky", "artwork", profile, app, role,
                                       "color" if downloaded else "muted", known)
            if not art or "png" not in art:
                return None
            data = base64.b64decode(art["png"], validate=True)
            state["pendingDigest"] = hashlib.sha256(data).hexdigest()
            state["pendingRevision"] = art["revision"]
            destination = None
            if role == "icon":
                destination = self._grid(account, appid, role)[0]
                state["pendingPath"] = str(destination)
                art = {"path": str(destination)}
            binding["artwork"][role] = state
            self._save(ledger)
            if destination is not None:
                destination.parent.mkdir(exist_ok=True)
                atomic(destination, data)
            return {"appid": appid, **art}
