import React, { useEffect, useState } from 'react';
import { callable, definePlugin } from '@decky/api';
import { ButtonItem, PanelSection, PanelSectionRow, TextField } from '@decky/ui';
import { artwork, reconcile, steamId } from './steam.ts';
import type { Bridge, CatalogApp } from './steam.ts';

type Profile = { id: string; origin: string; name: string; authenticated: boolean; status: string; lastSync: number | null };
type Status = { profiles: Profile[]; library: string; usage: { writableInstallationsLogicalBytes: number }; version: string };
type Steam = Parameters<typeof reconcile>[1];
const environment = window as unknown as {
  SteamClient?: { Apps?: Steam };
  App?: { m_CurrentUser?: { strSteamID?: string } };
};
const account = () => steamId(environment.App?.m_CurrentUser?.strSteamID ?? '');
const steam = () => environment.SteamClient?.Apps ?? {};
const bridgeReady = () => Boolean(steam().AddShortcut && steam().RegisterForAppDetails);
const statusRpc = callable<[], Status>('status');
const test = callable<[string, string], { connected: boolean }>('test');
const configure = callable<[string, string], string>('configure');
const catalog = callable<[string, boolean], CatalogApp[]>('catalog');
const rpc: Bridge = {
  shortcut: callable('shortcut'),
  acknowledge: callable('acknowledge'),
  binding: callable('binding'),
  artwork: callable('artwork'),
};

let lastOperations = 'Steam synchronization pending';
let work: Promise<void> | undefined;
const installed = new Map<string, boolean>();
const wait = () => new Promise<void>(resolve => setTimeout(resolve, 650));
const refresh = (profile: string): Promise<void> => {
  if (work) return work.catch(() => {}).then(() => refresh(profile));
  work = (async () => {
    const id = account();
    const rows = await catalog(profile, true);
    if (account() !== id) throw new Error('Steam account changed during catalog refresh');
    const problems = await reconcile(rpc, steam(), account, profile, rows, wait);
    lastOperations = problems.length ? problems.slice(0, 8).join('\n') : `${rows.filter(row => !row.stale).length} Steam shortcuts reconciled`;
    for (const row of rows) installed.set(`${id}/${profile}/${row.id}`, row.downloaded);
  })().finally(() => { work = undefined; });
  return work;
};

async function observeInstallations() {
  if (work) return;
  work = (async () => {
    const id = account();
    for (const profile of (await statusRpc()).profiles) {
      for (const row of await catalog(profile.id, false)) {
        const key = `${id}/${profile.id}/${row.id}`;
        if (row.stale || !installed.has(key) || installed.get(key) === row.downloaded) continue;
        const problems = await artwork(rpc, steam(), account, profile.id, row, id);
        if (problems.length) lastOperations = problems.slice(0, 8).join('\n');
        else installed.set(key, row.downloaded);
      }
    }
  })().finally(() => { work = undefined; });
  return work;
}

function AppDepot() {
  const [status, setStatus] = useState<Status>();
  const [rows, setRows] = useState<CatalogApp[]>([]);
  const [origin, setOrigin] = useState('');
  const [token, setToken] = useState('');
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);

  async function update() {
    const info = await statusRpc();
    setStatus(info);
    const groups = await Promise.all(info.profiles.map(profile => catalog(profile.id, false)));
    setRows(groups.flat());
  }

  useEffect(() => {
    const check = () => { void update().catch(error => setMessage(String(error))); };
    check();
    const timer = setInterval(check, 30_000);
    return () => clearInterval(timer);
  }, []);

  async function operation(action: () => Promise<void>) {
    setBusy(true);
    setMessage('');
    try {
      await action();
      await update();
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  }

  return <>
    <PanelSection title="Server">
      <PanelSectionRow><TextField label="Server URL" value={origin} onChange={event => setOrigin(event.target.value)} /></PanelSectionRow>
      <PanelSectionRow><TextField label="Access token (optional)" bIsPassword value={token} onChange={event => setToken(event.target.value)} /></PanelSectionRow>
      <PanelSectionRow><ButtonItem layout="below" disabled={busy || !origin} onClick={() => operation(async () => {
        await test(origin.trim(), token);
        setMessage('Connection and catalog access verified.');
      })}>Test connection</ButtonItem></PanelSectionRow>
      <PanelSectionRow><ButtonItem layout="below" disabled={busy || !origin} onClick={() => operation(async () => {
        const profile = await configure(origin.trim(), token);
        setToken('');
        await refresh(profile);
        setMessage(lastOperations);
      })}>Save and synchronize</ButtonItem></PanelSectionRow>
      {status?.profiles.map(profile => <PanelSectionRow key={profile.id}>
        <ButtonItem layout="below" disabled={busy} description={`${profile.status} · ${profile.lastSync ? new Date(profile.lastSync).toLocaleString() : 'Never synchronized'}`}
          onClick={() => operation(async () => { await refresh(profile.id); setMessage(lastOperations); })}>
          Synchronize {profile.origin}
        </ButtonItem>
      </PanelSectionRow>)}
      {message && <PanelSectionRow><div role="status" style={{ whiteSpace: 'pre-wrap' }}>{message}</div></PanelSectionRow>}
    </PanelSection>
    <PanelSection title="Installed">
      {rows.filter(row => row.installations.length).map(row => <PanelSectionRow key={row.installations[0].id}>
        {row.name} · {row.installations.map(item => item.state).join(', ')}
      </PanelSectionRow>)}
      {!rows.some(row => row.installations.length) && <PanelSectionRow>No local installations</PanelSectionRow>}
    </PanelSection>
    <PanelSection title="Settings">
      <PanelSectionRow>{status?.library}</PanelSectionRow>
      <PanelSectionRow>Runtime storage · {((status?.usage.writableInstallationsLogicalBytes ?? 0) / 1048576).toFixed(0)} MiB</PanelSectionRow>
      <PanelSectionRow>App Depot {status?.version} · {bridgeReady() ? 'Steam bridge available' : 'Steam bridge unavailable'}</PanelSectionRow>
      <PanelSectionRow><div style={{ whiteSpace: 'pre-wrap' }}>{lastOperations}</div></PanelSectionRow>
    </PanelSection>
  </>;
}

export default definePlugin(() => {
  let stopped = false;
  const start = async () => {
    try {
      account();
      if (!bridgeReady()) throw new Error('Steam bridge is not ready. Synchronize from App Depot.');
      for (const profile of (await statusRpc()).profiles) {
        if (stopped) return;
        await refresh(profile.id);
      }
    } catch (error) {
      lastOperations = String(error);
    }
  };
  const initial = setTimeout(() => { void start(); }, 1800);
  const timer = setInterval(() => {
    void observeInstallations()?.catch(error => { lastOperations = String(error); });
  }, 30_000);
  return {
    name: 'App Depot', titleView: <div>App Depot</div>,
    content: <AppDepot />, icon: <span aria-hidden>↓</span>,
    onDismount: () => { stopped = true; clearInterval(timer); clearTimeout(initial); },
  };
});
