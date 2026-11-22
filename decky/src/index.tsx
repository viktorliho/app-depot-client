import React, { useEffect, useRef, useState } from 'react';
import { callable, definePlugin } from '@decky/api';
import { ButtonItem, ConfirmModal, PanelSection, PanelSectionRow, showModal, Spinner, TextField, ToggleField } from '@decky/ui';
import { artwork, reconcile, steamId } from './steam.ts';
import type { Bridge, CatalogApp } from './steam.ts';
import { describeInstalled, removalPrompt } from './installed.ts';

type Profile = { id: string; origin: string; name: string; authenticated: boolean; status: string; lastSync: number | null };
type Status = { profiles: Profile[]; library: string; usage: { writableInstallationsLogicalBytes: number }; version: string; coreVersion: string };
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
  uninstall_busy: 'Close the game and pause or cancel active downloads before uninstalling.',
  uninstall_incomplete: 'Uninstall could not finish safely. Reopen Installed to retry recovery.',
  uninstall_stale: 'Local content changed. Refresh Installed and confirm the current selection.',
  update_busy: 'An update check or verification is already running.',
  update_rate_limited: 'GitHub is limiting release checks. Try again later.',
  update_tls_failed: 'The GitHub TLS certificate could not be verified.',
  update_network_failed: 'Could not reach GitHub. Check your network connection and retry.',
  update_metadata_invalid: 'GitHub returned invalid Decky release information.',
  update_http_failed: 'GitHub rejected the release check. Try again later.',
  package_metadata_invalid: 'The installed Decky package metadata is invalid. Reinstall from Desktop Mode.',
  shortcut_ownership: 'Steam shortcut ownership could not be verified. The entry was preserved.',
  shortcut_ambiguous: 'Steam returned conflicting shortcut identities. The entries were preserved.',
  shortcut_changed: 'Steam shortcut settings changed during inspection. Synchronize again.',
  shortcut_unconfirmed: 'Steam has not confirmed this shortcut. Synchronize again after Steam saves the library.',
  shortcut_identity: 'Steam returned an unexpected shortcut ID.',
  shortcut_storage: 'Steam account storage is unavailable.',
  shortcut_launcher: 'The shortcut launcher path could not be verified.',
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
type LocalApp = { id: string; name: string; profile: string; profileName: string; origin: string;
  revision: string; states: string[]; runtimeBytes: number; downloadBytes: number; blocked: boolean };
const localApps = rpcMethod<[], LocalApp[]>('installed');
const uninstall = rpcMethod<[string, string, string], { removed: boolean; cleanupComplete: boolean }>('uninstall');
const gridStates = rpcMethod<[string], { profile: string; app: string; modes: boolean[]; pending: boolean }[]>('grid_states');
const finishGrids = rpcMethod<[string, string, string], void>('finish_grids');
const gridRoles = ['steam.grid.vertical', 'steam.grid.horizontal'] as const;
const rpc: Bridge = {
  shortcut: rpcMethod('shortcut'),
  acknowledge: rpcMethod('acknowledge'),
  binding: rpcMethod('binding'),
  artwork: rpcMethod('artwork'),
};

type Background = { sync: string; artwork: string };
const background: Background = { sync: '', artwork: '' };
const backgroundListeners = new Set<(value: Background) => void>();
function reportBackground(kind: keyof Background, detail: string) {
  background[kind] = detail;
  for (const listener of backgroundListeners) listener({ ...background });
}
function diagnostic(error: unknown) {
  return error instanceof ConnectionFailure ? error.code : 'unexpected_error';
}
let lastOperations = 'Steam synchronization pending';
let work: Promise<void> | undefined;
const installed = new Map<string, boolean>();
const artworkRetry = new Set<string>();
const wait = () => new Promise<void>(resolve => setTimeout(resolve, 650));
const refresh = (profile: string): Promise<void> => {
  if (work) return work.catch(() => {}).then(() => refresh(profile));
  work = (async () => {
    const id = account();
    const rows = await catalog(profile, true);
    if (account() !== id) throw new Error('Steam account changed during catalog refresh');
    const artIssues: string[] = [];
    const problems = await reconcile(rpc, steam(), account, profile, rows, wait, (app, issues) => {
      const key = `${id}/${profile}/${app}`;
      if (issues.length) {
        artIssues.push(...issues);
        artworkRetry.add(key);
      } else artworkRetry.delete(key);
    });
    reportBackground('artwork', artIssues.length ? `${artIssues.length} artwork issue(s) · ${artIssues[0]}` : '');
    reportBackground('sync', problems.length ? `${problems.length} Steam reconciliation issue(s) · ${problems[0]}` : '');
    lastOperations = problems.length ? 'Synchronization finished with issues. See Diagnostics.' : `${rows.filter(row => !row.stale).length} Steam shortcuts reconciled`;
    for (const row of rows) installed.set(`${id}/${profile}/${row.id}`, row.downloaded);
  })().finally(() => { work = undefined; });
  return work;
};

async function observeInstallations() {
  if (work) return;
  work = (async () => {
    const id = account();
    const states = await gridStates(id);
    const issues: string[] = [];
    for (const profile of (await statusRpc()).profiles) {
      const rows = await catalog(profile.id, false);
      const bindings = states.filter(item => item.profile === profile.id);
      const pendingForProfile = bindings.filter(item => item.pending);
      for (const { app } of pendingForProfile) {
        const row = rows.find(item => item.id === app);
        if (row?.downloaded || row?.installations.some(item => ['Preparing', 'Removing'].includes(item.state))) continue;
        const problems = await artwork(rpc, steam(), account, profile.id,
          { id: app, name: row?.name ?? 'App Depot', downloaded: false }, id, gridRoles);
        if (problems.length) issues.push(...problems);
        else {
          if (account() !== id) throw new Error('Steam account changed during artwork update');
          await finishGrids(id, profile.id, app);
          artworkRetry.delete(`${id}/${profile.id}/${app}`);
          installed.set(`${id}/${profile.id}/${app}`, false);
        }
      }
      for (const row of rows) {
        if (pendingForProfile.some(item => item.app === row.id) && !row.downloaded) continue;
        const key = `${id}/${profile.id}/${row.id}`;
        const modeChanged = installed.has(key)
          ? installed.get(key) !== row.downloaded
          : bindings.find(item => item.app === row.id)?.modes.some(mode => mode !== row.downloaded);
        if (row.stale || (!modeChanged && !artworkRetry.has(key))) continue;
        const problems = await artwork(rpc, steam(), account, profile.id, row, id);
        if (problems.length) issues.push(...problems);
        else {
          artworkRetry.delete(key);
          installed.set(key, row.downloaded);
        }
      }
    }
    reportBackground('artwork', issues.length ? `${issues.length} artwork update issue(s) · ${issues[0]}` : '');
  })().catch(error => {
    reportBackground('artwork', diagnostic(error));
  }).finally(() => { work = undefined; });
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
    <PanelSectionRow>{version ? `App Depot Decky ${version}` : 'Loading version…'}</PanelSectionRow>
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
  const [rows, setRows] = useState<LocalApp[]>([]);
  const [removal, setRemoval] = useState<{ app: LocalApp; started: number }>();
  const [elapsed, setElapsed] = useState(0);
  const removing = useRef(false);
  const [removeResult, setRemoveResult] = useState('');
  const [diagnostics, setDiagnostics] = useState({ ...background });
  const [inventoryIssue, setInventoryIssue] = useState('');
  const [showDiagnostics, setShowDiagnostics] = useState(false);
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
    setRows(await localApps());
  }

  useEffect(() => {
    backgroundListeners.add(setDiagnostics);
    const check = () => { void update().then(() => setInventoryIssue(''), error => setInventoryIssue(diagnostic(error))); };
    check();
    const timer = setInterval(check, 30_000);
    return () => { backgroundListeners.delete(setDiagnostics); clearInterval(timer); };
  }, []);

  useEffect(() => {
    if (!removal) return;
    const timer = setInterval(() => setElapsed(Math.floor((performance.now() - removal.started) / 1000)), 1000);
    return () => clearInterval(timer);
  }, [removal]);

  async function confirmRemoval(app: LocalApp) {
    if (removing.current) return;
    removing.current = true;
    setElapsed(0);
    setRemoval({ app, started: performance.now() });
    setRemoveResult('');
    try {
      const result = await uninstall(app.profile, app.id, app.revision);
      setRows(current => current.filter(row => row.id !== app.id || row.profile !== app.profile));
      setRemoveResult(result.cleanupComplete ? `${app.name} uninstalled. Some unused storage could not be reclaimed.`
        : `${app.name} uninstalled.`);
      void observeInstallations();
    } catch (error) {
      setRemoveResult(`${app.name}: ${error instanceof ConnectionFailure && error.code !== 'internal'
        ? error.message : 'Uninstall could not be confirmed. Refresh Installed before retrying.'}`);
    } finally {
      setRemoval(undefined);
      removing.current = false;
      void update().then(() => setInventoryIssue(''), error => setInventoryIssue(diagnostic(error)));
    }
  }

  function askUninstall(app: LocalApp) {
    if (removing.current) return;
    const modal = showModal(<ConfirmModal
      {...removalPrompt(app.name)}
      bDestructiveWarning strOKButtonText="Uninstall" strCancelButtonText="Cancel"
      onOK={() => { modal.Close(); void confirmRemoval(app); }}
      onCancel={() => modal.Close()} />);
  }

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
      {(removal && !rows.some(row => row.id === removal.app.id && row.profile === removal.app.profile)
        ? [...rows, removal.app] : rows).map(row => {
        const item = describeInstalled(row, removal?.app, elapsed);
        return <PanelSectionRow key={item.key}>
          <ButtonItem layout="inline" label={<span style={{ overflowWrap: 'anywhere' }}>{item.label}</span>}
            description={item.active ? <span role="status" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
              <Spinner style={{ width: 16, height: 16 }} /> {item.description}
            </span> : item.description}
            disabled={item.disabled} onClick={() => askUninstall(row)}>Uninstall</ButtonItem>
        </PanelSectionRow>;
      })}
      {!rows.length && !removal && <PanelSectionRow>{inventoryIssue ? 'Installed list unavailable' : 'No local installations'}</PanelSectionRow>}
      {removeResult && <PanelSectionRow><div role="status">{removeResult}</div></PanelSectionRow>}
    </PanelSection>
    <PanelSection title="Settings">
      <PanelSectionRow>{status?.library}</PanelSectionRow>
      <PanelSectionRow>Runtime storage · {((status?.usage.writableInstallationsLogicalBytes ?? 0) / 1048576).toFixed(0)} MiB</PanelSectionRow>
      <PanelSectionRow>{status ? `App Depot Decky ${status.version}` : 'Loading version…'}</PanelSectionRow>
      {diagnostics.sync && <PanelSectionRow>Steam synchronization needs attention.</PanelSectionRow>}
      {diagnostics.artwork && <PanelSectionRow>Artwork refresh deferred. App Depot will retry.</PanelSectionRow>}
      <PanelSectionRow><ToggleField label="Diagnostics" checked={showDiagnostics} onChange={setShowDiagnostics} /></PanelSectionRow>
      {showDiagnostics && <PanelSectionRow><div style={{ overflowWrap: 'anywhere' }}>
        <div>Core {status?.coreVersion ?? 'unavailable'} · {bridgeReady() ? 'Steam bridge available' : 'Steam bridge unavailable'}</div>
        <div>Sync · {diagnostics.sync || lastOperations}</div>
        {diagnostics.artwork && <div>Artwork · {diagnostics.artwork}</div>}
        {inventoryIssue && <div>Installed list · {inventoryIssue}</div>}
      </div></PanelSectionRow>}
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
      reportBackground('sync', diagnostic(error));
    }
  };
  const initial = setTimeout(() => { void start(); }, 1800);
  const timer = setInterval(() => {
    void observeInstallations();
  }, 30_000);
  return {
    name: 'App Depot', titleView: <div>App Depot</div>,
    content: <AppDepot />, icon: <span aria-hidden>↓</span>,
    onDismount: () => { stopped = true; clearInterval(timer); clearTimeout(initial); },
  };
});
