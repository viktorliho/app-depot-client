import React, { useEffect, useState } from 'react';
import { callable, definePlugin } from '@decky/api';
import { ButtonItem, PanelSection, PanelSectionRow, TextField, ToggleField } from '@decky/ui';
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
type Candidate = { name: string; origin: string };
type Probe = { connected: boolean; guest: boolean; appCount: number };
type Result<T> = { ok: true; value: T } | { ok: false; code: string };
const request = callable<[string, ...unknown[]], Result<unknown>>('request');
const messages: Record<string, string> = {
  authentication_required: 'This server requires authentication to access its catalog.',
  anonymous_reading_disabled: 'This server requires authentication to access its catalog. Use an access token or ask the administrator to enable anonymous reading.',
  invalid_token: 'This server rejected the access token. Check it and try again.',
  invalid_url: 'Enter a valid HTTP or HTTPS Server address.',
  http_consent_required: 'Enable Allow unencrypted HTTP before connecting to this address.',
  catalog_forbidden: 'This account is not allowed to read the catalog.',
  unsupported_protocol: 'This endpoint does not provide a compatible App Depot Server API.',
  server_unavailable: 'The Server is unavailable. Check the address and try again.',
  timeout: 'The Server did not respond in time. Try again.',
  tls_failure: 'The HTTPS certificate or TLS connection could not be verified.',
  server_error: 'The Server could not complete this request.',
  secret_service_unavailable: 'Secret Service is unavailable or the saved credential is missing. Unlock the user keyring and try again.',
  saved_credentials: 'This Server already has a saved token. Synchronize its existing profile to keep that configuration.',
  discovery_unavailable: 'LAN discovery is unavailable. Enter the Server address manually.',
  update_busy: 'An update check or verification is already running.',
  update_rate_limited: 'GitHub is limiting release checks. Try again later.',
  update_check_failed: 'Could not check official Decky releases. Try again.',
  update_verification_failed: 'The update could not be downloaded or verified. Retry or check again.',
  internal: 'App Depot could not complete this operation. Check the plugin diagnostics.',
};

class ConnectionFailure extends Error {
  constructor(code: string) { super(messages[code] ?? messages.internal); this.code = code; }
  readonly code: string;
}

function rpcMethod<A extends unknown[], T>(method: string) {
  return async (...args: A): Promise<T> => {
    let result: Result<unknown>;
    try {
      result = await request(method, ...args);
    } catch {
      throw new ConnectionFailure('internal');
    }
    if (!result.ok) throw new ConnectionFailure(result.code);
    return result.value as T;
  };
}
const statusRpc = rpcMethod<[], Status>('status');
const discover = rpcMethod<[], Candidate[]>('discover');
const test = rpcMethod<[string, string, boolean], Probe>('test');
const configure = rpcMethod<[string, string, boolean], string>('configure');
const catalog = rpcMethod<[string, boolean], CatalogApp[]>('catalog');
const rpc: Bridge = {
  shortcut: rpcMethod('shortcut'),
  acknowledge: rpcMethod('acknowledge'),
  binding: rpcMethod('binding'),
  artwork: rpcMethod('artwork'),
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

type Update = { version: string; notes: string; url: string };
const checkUpdate = rpcMethod<[], Update | null>('check_update');
const verifyUpdate = rpcMethod<[], null>('verify_update');

function Updates({ version }: { version?: string }) {
  const [update, setUpdate] = useState<Update | null>(null);
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(false);

  const check = async () => {
    setBusy(true);
    setUpdate(null);
    setMessage('Checking for updates…');
    try {
      const result = await checkUpdate();
      setUpdate(result);
      setMessage(result ? 'Update available' : 'Up to date');
    } catch (error) {
      setMessage(error instanceof ConnectionFailure ? error.message : messages.update_check_failed);
    } finally {
      setBusy(false);
    }
  };

  const verify = async () => {
    setBusy(true);
    setMessage('Downloading and verifying…');
    try {
      await verifyUpdate();
      setMessage('Official package verified. Use the release’s installer in Desktop Mode to upgrade.');
    } catch (error) {
      setMessage(error instanceof ConnectionFailure ? error.message : messages.update_verification_failed);
    } finally {
      setBusy(false);
    }
  };

  return <PanelSection title="Updates & About">
    <PanelSectionRow>Installed version · {version ?? 'Loading…'}</PanelSectionRow>
    <PanelSectionRow>Gaming Mode can check and verify updates. To install an update, use its installer in Desktop Mode.</PanelSectionRow>
    <PanelSectionRow><ButtonItem disabled={busy} onClick={check}>Check for updates</ButtonItem></PanelSectionRow>
    {message && <PanelSectionRow><div role="status">{message}</div></PanelSectionRow>}
    {update && <>
      <PanelSectionRow>Available version · {update.version}</PanelSectionRow>
      {update.notes && <PanelSectionRow><div style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>{update.notes}</div></PanelSectionRow>}
      <PanelSectionRow><ButtonItem disabled={busy} onClick={verify}>Download and verify</ButtonItem></PanelSectionRow>
      <PanelSectionRow><div style={{ overflowWrap: 'anywhere' }}>Desktop installer · {update.url}</div></PanelSectionRow>
    </>}
  </PanelSection>;
}

function AppDepot() {
  const [status, setStatus] = useState<Status>();
  const [rows, setRows] = useState<CatalogApp[]>([]);
  const [origin, setOrigin] = useState('');
  const [token, setToken] = useState('');
  const [useToken, setUseToken] = useState(false);
  const [manual, setManual] = useState(false);
  const [selected, setSelected] = useState<Candidate>();
  const [candidates, setCandidates] = useState<Candidate[]>([]);
  const [searching, setSearching] = useState(false);
  const [verified, setVerified] = useState<Record<string, string>>({});
  const [searched, setSearched] = useState(false);
  const [discoveryError, setDiscoveryError] = useState('');
  const [allowHttp, setAllowHttp] = useState(false);
  const isHttp = /^http:\/\//i.test(origin.trim());
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

  async function search() {
    setSearching(true);
    setSearched(true);
    setCandidates([]);
    setDiscoveryError('');
    try {
      setCandidates(await discover());
    } catch (error) {
      setDiscoveryError(error instanceof Error ? error.message : messages.internal);
    } finally {
      setSearching(false);
    }
  }

  useEffect(() => {
    if (status && status.profiles.length === 0 && !searched) void search();
  }, [status, searched]);

  function choose(candidate?: Candidate) {
    setSelected(candidate);
    setManual(!candidate);
    setOrigin(candidate?.origin ?? '');
    setUseToken(false);
    setToken('');
    setAllowHttp(false);
    setMessage('');
  }

  async function operation(action: () => Promise<void>) {
    setBusy(true);
    setMessage('');
    try {
      await action();
      await update();
    } catch (error) {
      if (error instanceof ConnectionFailure && error.code === 'anonymous_reading_disabled') setUseToken(true);
      setMessage(error instanceof Error ? error.message : messages.internal);
    } finally {
      setBusy(false);
    }
  }

  return <>
    <PanelSection title="Server">
      {searching && <PanelSectionRow><div role="status">Searching for App Depot Servers…</div></PanelSectionRow>}
      {discoveryError && <PanelSectionRow>{discoveryError}</PanelSectionRow>}
      {searched && !searching && !discoveryError && !candidates.length && <PanelSectionRow>No Servers discovered</PanelSectionRow>}
      {candidates.map(candidate => <PanelSectionRow key={candidate.origin}>
        <ButtonItem layout="below" disabled={busy} description={`${candidate.origin} · ${verified[candidate.origin] ?? 'Unverified'}`}
          onClick={() => choose(candidate)}>{candidate.name}</ButtonItem>
      </PanelSectionRow>)}
      <PanelSectionRow><ButtonItem layout="below" disabled={searching || busy} onClick={search}>Discover Servers</ButtonItem></PanelSectionRow>
      <PanelSectionRow><ButtonItem layout="below" disabled={busy} onClick={() => choose()}>Enter address manually</ButtonItem></PanelSectionRow>
      {(selected || manual) && <>
        {manual ? <PanelSectionRow><TextField label="Server URL" value={origin}
          onChange={event => { setOrigin(event.target.value); setAllowHttp(false); setToken(''); setMessage(''); }} /></PanelSectionRow>
          : <PanelSectionRow>{selected?.origin}</PanelSectionRow>}
        {isHttp && <PanelSectionRow><ToggleField label="Allow unencrypted HTTP"
          description="HTTP sends access tokens without transport encryption. Only enable this if you accept that someone on the network could read the token."
          checked={allowHttp} onChange={setAllowHttp} disabled={busy} /></PanelSectionRow>}
        <PanelSectionRow><ToggleField label="Use an access token" checked={useToken} disabled={busy}
          description="Guest access includes only Public Published items. Use a token to access other items you have permission to read."
          onChange={checked => { setUseToken(checked); if (!checked) setToken(''); }} /></PanelSectionRow>
        {useToken && <PanelSectionRow><TextField label="Access token" bIsPassword value={token}
          onChange={event => setToken(event.target.value)} /></PanelSectionRow>}
        <PanelSectionRow><ButtonItem layout="below" disabled={busy || !origin || (useToken && !token)}
          onClick={() => operation(async () => {
            const result = await test(origin.trim(), useToken ? token : '', isHttp && allowHttp);
            setVerified(previous => ({ ...previous, [origin.trim()]: result.guest ? 'Guest verified' : 'Token verified' }));
            setMessage(result.guest
              ? `Guest access verified. ${result.appCount} Public Published Apps are currently accessible.`
              : 'Authenticated catalog access verified.');
          })}>Test connection</ButtonItem></PanelSectionRow>
        <PanelSectionRow><ButtonItem layout="below" disabled={busy || !origin || (useToken && !token)}
          onClick={() => operation(async () => {
            const profile = await configure(origin.trim(), useToken ? token : '', isHttp && allowHttp);
            setToken('');
            setVerified(previous => ({ ...previous, [origin.trim()]: useToken ? 'Token verified' : 'Guest verified' }));
            await refresh(profile);
            setMessage(useToken ? lastOperations : `Guest · ${lastOperations}. Guest access includes only Public Published items.`);
          })}>Save and synchronize</ButtonItem></PanelSectionRow>
      </>}
      {status?.profiles.map(profile => <PanelSectionRow key={profile.id}>
        <ButtonItem layout="below" disabled={busy} description={`${profile.origin.startsWith("http://") ? "HTTP · unencrypted · " : ""}${profile.authenticated ? "Token" : "Guest"} · ${profile.status} · ${profile.lastSync ? new Date(profile.lastSync).toLocaleString() : 'Never synchronized'}`}
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
    <Updates version={status?.version} />
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
