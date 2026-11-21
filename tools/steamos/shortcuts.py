"""Steam shortcut reconciliation for disposable binary VDF fixtures."""

from collections import defaultdict
from dataclasses import dataclass
from uuid import UUID
from zlib import crc32

import vdf
from PIL import Image, ImageOps


@dataclass(frozen=True)
class Binding:
    appid: int
    last_name: str


def shortcut_id(exe, name):
    return crc32((exe + name).encode("utf-8")) | 0x80000000


def unique_fields(node):
    if isinstance(node, vdf.VDFDict):
        result = {}
        for key, value in node.items():
            if key in result:
                raise ValueError("duplicate binary VDF field")
            result[key] = unique_fields(value)
        return result
    return node


def fixture_target(profile, app):
    return f'"/fixture/launchers/{profile}/{app}/app-depot-wrapper"'


def reconcile(payload, profile_id, apps, bindings):
    """Reconcile a complete profile catalog into fixture bytes and a new ledger.

    The caller scopes the ledger to one Steam account and profile. It must
    persist the returned binding with the fixture as one logical operation.
    Missing catalog entries are retained for offline use.
    """
    if len(payload) > 8 * 1024 * 1024:
        raise ValueError("fixture exceeds VDF size limit")
    profile = UUID(profile_id)
    document = unique_fields(vdf.binary_loads(
        payload, mapper=vdf.VDFDict, merge_duplicate_keys=False,
    ))
    shortcuts = document["shortcuts"]
    by_id = {}
    by_marker = defaultdict(set)
    for _, record in shortcuts.items():
        appid = record["appid"] & 0xFFFFFFFF
        if appid in by_id:
            raise ValueError("ambiguous Steam AppID")
        by_id[appid] = record
        for tag in record.get("tags", {}).values():
            by_marker[tag].add(appid)
    ledger = dict(bindings)
    indexes = {key for key, _ in shortcuts.items()}
    next_index = 0
    for app_id, name in sorted(apps.items()):
        app = UUID(app_id)
        if str(app) != app_id:
            raise ValueError("expected canonical App ID")
        if not name or "\0" in name:
            raise ValueError("invalid shortcut name")
        exe = fixture_target(profile, app)
        marker = f"app-depot:{profile}:{app}"
        binding = ledger.get(app_id)
        if binding is not None:
            record = by_id.get(binding.appid)
            if record is None or by_marker[marker] != {binding.appid}:
                raise ValueError("saved shortcut ownership could not be confirmed")
            if record["AppName"] == binding.last_name and name != binding.last_name:
                record["AppName"] = name
                ledger[app_id] = Binding(binding.appid, name)
            continue
        if by_marker[marker]:
            raise ValueError("marker exists without a saved binding")
        appid = shortcut_id(exe, name)
        if appid in by_id:
            raise ValueError("Steam AppID collision; keep existing entry")
        while str(next_index) in indexes:
            next_index += 1
        record = {
            "appid": appid - (1 << 32), "AppName": name, "Exe": exe,
            "StartDir": '"/fixture"', "LaunchOptions": f"{profile} {app}",
            "tags": {"0": marker},
        }
        shortcuts[str(next_index)] = record
        indexes.add(str(next_index))
        by_id[appid] = record
        by_marker[marker].add(appid)
        ledger[app_id] = Binding(appid, name)
    return vdf.binary_dumps(document), ledger


def artwork_name(appid, role):
    if not 0x80000000 <= appid <= 0xFFFFFFFF:
        raise ValueError("expected unsigned non-Steam shortcut AppID")
    suffix = {"vertical": "p", "horizontal": "", "hero": "_hero", "logo": "_logo"}
    return f"{appid}{suffix[role]}.png"


def muted_grid(source):
    """Produce an independent RGBA derivative, preserving size and alpha."""
    rgba = source.convert("RGBA")
    gray = ImageOps.grayscale(rgba)
    return Image.merge("RGBA", (gray, gray, gray, rgba.getchannel("A")))
