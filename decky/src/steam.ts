export type CatalogApp = {
  id: string;
  name: string;
  stale: boolean;
  downloaded: boolean;
  installations: { id: string; state: string }[];
};

export type Plan =
  | { action: 'create'; target: string; options: string }
  | { action: 'rename'; appid: number; name: string }
  | { action: 'keep'; appid: number }
  | { action: 'inspect'; appid: number }
  | { action: 'pending' };

export const roles = ['steam.grid.vertical', 'steam.grid.horizontal', 'steam.hero', 'logo', 'icon'] as const;
type Role = typeof roles[number];
export type Observation = { appid: number; appname: string; exe: string; launchoptions: string };
type Details = { unAppID: number; strDisplayName: string; strShortcutExe: string; strShortcutLaunchOptions: string };
type Art = { appid: number; png?: string; path?: string };
type SteamMethods = {
  RegisterForAppDetails?(id: number, callback: (details: Details) => void): { unregister(): void };
  AddShortcut?(name: string, target: string, cwd: string, options: string): Promise<number>;
  SetShortcutName?(id: number, name: string): Promise<unknown>;
  SetCustomArtworkForApp?(id: number, png: string, format: string, role: number): Promise<unknown>;
  SetShortcutIcon?(id: number, path: string): Promise<unknown>;
};

export interface Bridge {
  shortcut(account: string, profile: string, app: string, name: string, observation?: Observation): Promise<Plan>;
  binding(account: string, profile: string, app: string): Promise<number | null>;
  acknowledge(account: string, profile: string, app: string, id: number): Promise<void>;
  artwork(account: string, profile: string, app: string, role: Role, downloaded: boolean, observation?: Observation): Promise<Art | null>;
}

export function steamId(raw: string): string {
  if (!/^\d{17}$/.test(raw)) throw new Error('Steam account unavailable');
  const identifier = BigInt(raw) - 76561197960265728n;
  if (identifier <= 0n || identifier > 0xffffffffn) throw new Error('Unsupported Steam account');
  return identifier.toString();
}

const types = { 'steam.grid.vertical': 0, 'steam.hero': 1, logo: 2, 'steam.grid.horizontal': 3 } as const;

function details(steam: SteamMethods, appid: number): Promise<Observation> {
  if (!steam.RegisterForAppDetails) return Promise.reject(new Error('Steam shortcut readback is unavailable'));
  return new Promise((resolve, reject) => {
    let request: { unregister(): void } | undefined;
    const timer = setTimeout(() => {
      request?.unregister();
      reject(new Error('Steam shortcut readback timed out'));
    }, 2500);
    try {
      request = steam.RegisterForAppDetails!(appid, value => {
        if (!value.strShortcutExe || (value.unAppID >>> 0) !== appid) return;
        clearTimeout(timer);
        queueMicrotask(() => request?.unregister());
        resolve({
          appid, appname: value.strDisplayName, exe: value.strShortcutExe, launchoptions: value.strShortcutLaunchOptions,
        });
      });
    } catch (error) {
      clearTimeout(timer);
      reject(error);
    }
  });
}

export async function reconcile(
  bridge: Bridge, steam: SteamMethods, currentAccount: () => string,
  profile: string, rows: CatalogApp[], wait: () => Promise<void>,
): Promise<string[]> {
  const account = currentAccount();
  const guard = () => {
    if (currentAccount() !== account) throw new Error('Steam account changed during reconciliation');
  };
  const issues: string[] = [];
  for (const row of rows) {
    if (row.stale) continue;
    try {
      guard();
      if (!steam.AddShortcut || !steam.RegisterForAppDetails) throw new Error('Steam shortcut API or readback is unavailable');
      const plan = (observed?: Observation) => observed
        ? bridge.shortcut(account, profile, row.id, row.name, observed)
        : bridge.shortcut(account, profile, row.id, row.name);
      let step = await plan();
      if (step.action === 'create') {
        guard();
        const id = await steam.AddShortcut(
          row.name, `"${step.target}"`, `"${step.target.substring(0, step.target.lastIndexOf('/'))}"`, step.options,
        );
        if (!Number.isInteger(id) || !((id >>> 0) & 0x80000000)) throw new Error('Steam returned an invalid shortcut ID');
        await bridge.acknowledge(account, profile, row.id, id >>> 0);
        await wait();
        step = await plan();
      }
      if (step.action === 'inspect') {
        const observed = await details(steam, step.appid);
        guard();
        step = await plan(observed);
      }
      if (step.action === 'pending') throw new Error('Shortcut creation is awaiting Steam readback. Refresh after Steam saves its library.');
      if (step.action === 'rename') {
        guard();
        if (!steam.SetShortcutName) throw new Error('Steam rename is unavailable');
        const changed = await steam.SetShortcutName(step.appid, step.name);
        if (changed === false) throw new Error('Steam rejected the rename');
        await wait();
        const readback = await plan(await details(steam, step.appid));
        if (readback.action !== 'keep' || readback.appid !== step.appid) throw new Error('Steam rename awaits readback');
      }
    } catch (error) {
      issues.push(`${row.name}: ${String(error)}`);
      continue;
    }
    issues.push(...await artwork(bridge, steam, currentAccount, profile, row, account));
  }
  return issues;
}

export async function artwork(
  bridge: Bridge, steam: SteamMethods, currentAccount: () => string,
  profile: string, row: CatalogApp, account: string,
): Promise<string[]> {
  const issues: string[] = [];
  let observed: Observation;
  try {
    const appid = await bridge.binding(account, profile, row.id);
    if (appid === null) return [];
    observed = await details(steam, appid);
  } catch (error) {
    return [`${row.name}: ${String(error)}`];
  }
  for (const role of roles) {
    try {
      if (currentAccount() !== account) throw new Error('Steam account changed during artwork update');
      if (role === 'icon' ? !steam.SetShortcutIcon : !steam.SetCustomArtworkForApp) continue;
      const art = await bridge.artwork(account, profile, row.id, role, row.downloaded, observed);
      if (!art) continue;
      if (currentAccount() !== account) throw new Error('Steam account changed during artwork update');
      const result = role === 'icon'
        ? await steam.SetShortcutIcon?.(art.appid, art.path!)
        : await steam.SetCustomArtworkForApp?.(art.appid, art.png!, 'png', types[role]);
      if (result === false) throw new Error('Steam rejected artwork');
    } catch (error) {
      issues.push(`${row.name} · ${role}: ${String(error)}`);
    }
  }
  return issues;
}
