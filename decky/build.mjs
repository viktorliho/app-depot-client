import { readFile } from 'node:fs/promises';
import { build } from 'esbuild';

await build({
  entryPoints: ['src/index.tsx'],
  bundle: true,
  format: 'esm',
  target: 'chrome120',
  outfile: 'dist/index.js',
  plugins: [{
    name: 'decky-globals',
    setup(api) {
      api.onResolve({ filter: /^(react|react-dom|@decky\/ui|@decky\/manifest)$/ }, args => ({ path: args.path, namespace: 'decky' }));
      api.onLoad({ filter: /.*/, namespace: 'decky' }, async args => {
        if (args.path === '@decky/manifest') return { contents: await readFile('plugin.json', 'utf8'), loader: 'json' };
        const global = { react: 'SP_REACT', 'react-dom': 'SP_REACTDOM', '@decky/ui': 'DFL' }[args.path];
        return { contents: `module.exports = window[${JSON.stringify(global)}];` };
      });
    },
  }],
});
