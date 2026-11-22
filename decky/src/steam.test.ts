import assert from 'node:assert/strict';
import { test } from 'node:test';
import { reconcile, steamId } from './steam.ts';
import type { Bridge, CatalogApp, Plan } from './steam.ts';

const app = (id: string, name: string): CatalogApp =>
  ({ id, name, stale: false, downloaded: false, installations: [] });
const wait = async () => {};

test('Steam account parsing preserves the 32-bit account number', () => {
  assert.equal(steamId('76561197960265851'), '123');
  assert.throws(() => steamId('invalid'));
  assert.throws(() => steamId('76561197960265728'));
});

test('create acknowledges the actual ID, rechecks, renames and leaves foreign entries out', async () => {
  const rows = [app('one', 'Same'), app('two', 'Same')];
  const bound = new Map<string, { id: number; name: string }>();
  let number = 0x80000004;
  let adds = 0;
  const mutations: string[] = [];
  const bridge: Bridge = {
    async shortcut(_account, profile, app, name): Promise<Plan> {
      const entry = bound.get(`${profile}/${app}`);
      if (!entry) return { action: 'create', target: `/fixture/${profile}/${app}/play`, options: `${profile}/${app}` };
      return entry.name === name ? { action: 'keep', appid: entry.id } : { action: 'rename', appid: entry.id, name };
    },
    async acknowledge(_account, profile, app, id) {
      assert.ok(id >= 0x80000000);
      bound.set(`${profile}/${app}`, { id, name: 'Same' });
    },
    async binding() { return null; },
    async artwork() { return null; },
  };
  const steam = {
    async AddShortcut(_name: string, target: string, options: string, commandLine: string) {
      assert.ok(target.includes(options));
      assert.equal(commandLine, `"${target}" ${options}`);
      adds++;
      return number++ | 0;
    },
    RegisterForAppDetails(id: number, callback: (details: {
      unAppID: number; strDisplayName: string; strShortcutExe: string; strShortcutLaunchOptions: string; strShortcutStartDir: string;
    }) => void) {
      const entry = [...bound.values()].find(value => value.id === id);
      assert.ok(entry);
      callback({ unAppID: id, strDisplayName: entry.name, strShortcutExe: 'fixture', strShortcutLaunchOptions: 'fixture', strShortcutStartDir: 'fixture' });
      return { unregister() {} };
    },
    async SetShortcutName(id: number, name: string) {
      const entry = [...bound.values()].find(value => value.id === id);
      assert.ok(entry);
      entry.name = name;
      mutations.push(`${id}/${name}`);
    },
  };
  for (const profile of ['first', 'first', 'second']) {
    assert.deepEqual(await reconcile(bridge, steam, () => '123', profile, rows, wait), []);
  }
  assert.equal(adds, 4);
  assert.deepEqual(await reconcile(bridge, steam, () => '123', 'first', [app('one', 'New')], wait), []);
  assert.deepEqual(mutations, ['2147483652/New']);
  assert.equal(adds, 4);
});

test('pending readback and account changes never trigger a second creation', async () => {
  let added = 0;
  const bridge: Bridge = {
    async shortcut() { return { action: 'pending' }; },
    async acknowledge() { throw new Error('unexpected acknowledgement'); },
    async binding() { throw new Error('unexpected binding'); },
    async artwork() { throw new Error('unexpected artwork'); },
  };
  const steam = {
    async AddShortcut() { added++; return 0x80000001; },
    RegisterForAppDetails() { throw new Error('unexpected readback'); },
  };
  const issues = await reconcile(bridge, steam, () => '123', 'profile', [app('a', 'App')], wait);
  assert.match(issues[0], /awaiting Steam readback/);
  let account = 124;
  const changed = await reconcile(bridge, steam, () => String(account++), 'profile', [app('b', 'Other')], wait);
  assert.match(changed[0], /account changed/);
  assert.equal(added, 0);
});

test('sets fields explicitly after Steam derives the name from the executable', async () => {
  const id = 0x80000042;
  const target = '/fixture/launchers/profile/app/play';
  const cwd = '/fixture/launchers/profile/app';
  const options = 'decky play profile app';
  let created = 0;
  let acknowledged = false;
  let initialized = false;
  let applied = 0;
  const live = {
    unAppID: id, strDisplayName: 'play', strShortcutExe: `"${target}"`,
    strShortcutStartDir: `${cwd}/`, strShortcutLaunchOptions: '',
  };
  const bridge: Bridge = {
    async shortcut(_account, _profile, _app, _name, observation) {
      if (!acknowledged) return { action: 'create', target, options };
      if (!observation) return { action: 'inspect', appid: id };
      assert.equal(observation.appid, id);
      if (observation.launchoptions !== options) return {
        action: 'setup', appid: id,
        fields: { appname: 'Fixture App', startdir: `"${cwd}"`, launchoptions: options },
      };
      assert.deepEqual(observation, {
        appid: id, appname: 'Fixture App', exe: `"${target}"`, startdir: `"${cwd}"`, launchoptions: options,
      });
      initialized = true;
      return { action: 'keep', appid: id };
    },
    async acknowledge(_a, _p, _app, actual) {
      assert.equal(actual, id);
      acknowledged = true;
    },
    async binding() { return initialized ? id : null; },
    async artwork(_a, _p, _app, role) {
      assert.ok(initialized);
      return role === 'steam.grid.vertical' ? { appid: id, png: 'fixture' } : null;
    },
  };
  const steam = {
    async AddShortcut(...args: string[]) {
      assert.deepEqual(args, ['Fixture App', target, options, `"${target}" ${options}`]);
      created++;
      return id | 0;
    },
    RegisterForAppDetails(actual: number, callback: (details: typeof live) => void) {
      assert.equal(actual, id);
      callback({ ...live });
      return { unregister() {} };
    },
    async SetShortcutName(actual: number, name: string) { assert.equal(actual, id); live.strDisplayName = name; },
    async SetShortcutStartDir(actual: number, dir: string) { assert.equal(actual, id); live.strShortcutStartDir = dir; },
    async SetShortcutLaunchOptions(actual: number, args: string) { assert.equal(actual, id); live.strShortcutLaunchOptions = args; },
    async SetCustomArtworkForApp(...args: unknown[]) {
      assert.deepEqual(args, [id, 'fixture', 'png', 0]);
      applied++;
    },
  };
  assert.deepEqual(await reconcile(bridge, steam, () => '123', 'profile', [app('app','Fixture App')], wait), []);
  assert.equal(created, 1);
  assert.equal(applied, 1);
});
