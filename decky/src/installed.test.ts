import assert from 'node:assert/strict';
import { test } from 'node:test';
import { describeInstalled, removalPrompt } from './installed.ts';

const app = { profile: 'one', id: 'app', name: 'Fixture', origin: 'https://private.invalid',
  runtimeBytes: 524288, downloadBytes: 524288, blocked: false };

test('Installed renders a compact label separately from action and omits server names and URLs', () => {
  assert.deepEqual(describeInstalled(app, undefined, 0), {
    key: 'one/app', label: 'Fixture', description: '1.0 MiB logical', active: false, disabled: false,
  });
  assert.equal(removalPrompt(app.name).strTitle, 'Uninstall Fixture?');
  assert.match(removalPrompt(app.name).strDescription, /permanently deleted.*Steam Library/);
});

test('elapsed progress stays on the selected profile and all duplicate actions are disabled', () => {
  assert.equal(describeInstalled(app, app, 20).description, 'Uninstalling… 20s');
  const other = describeInstalled({ ...app, profile: 'two' }, app, 20);
  assert.equal(other.active, false);
  assert.equal(other.disabled, true);
  assert.equal(describeInstalled({ ...app, blocked: true }, undefined, 0).description, 'Active operation');
});
