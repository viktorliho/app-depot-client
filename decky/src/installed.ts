type Identity = { profile: string; id: string };
type Installed = Identity & { name: string; runtimeBytes: number; downloadBytes: number; blocked: boolean };

export function describeInstalled(
  app: Installed, removing: Identity | undefined, seconds: number,
) {
  const active = app.id === removing?.id && app.profile === removing.profile;
  const usage = ((app.runtimeBytes + app.downloadBytes) / 1048576).toFixed(1);
  return {
    key: `${app.profile}/${app.id}`,
    label: app.name,
    description: active ? `Uninstalling… ${seconds}s`
      : app.blocked ? 'Active operation' : `${usage} MiB logical`,
    disabled: Boolean(removing) || app.blocked,
    active,
  };
}

export function removalPrompt(name: string) {
  return {
    strTitle: `Uninstall ${name}?`,
    strDescription: 'Local application files and data may be permanently deleted. The game will remain in Steam Library and can be downloaded again.',
  };
}
