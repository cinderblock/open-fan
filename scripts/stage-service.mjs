/**
 * Build the OpenFan service and stage it where the bundler expects to find it.
 *
 * Tauri's `externalBin` looks for `binaries/<name>-<target-triple>.exe`, so the binary
 * cannot simply be pointed at from `target/release`. This builds it and copies it under
 * the name the bundler wants.
 *
 * Staging is generated output, not source: `src-tauri/binaries/` is gitignored, and the
 * only way to get a stale service into an installer is to skip this script.
 */
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');

/** The triple the bundler will look for, taken from rustc rather than assumed. */
function hostTriple() {
  const out = execFileSync('rustc', ['-vV'], { encoding: 'utf8' });
  const line = out.split('\n').find((l) => l.startsWith('host:'));
  if (!line) throw new Error('could not read the host triple from `rustc -vV`');
  return line.slice('host:'.length).trim();
}

const profile = process.argv.includes('--debug') ? 'debug' : 'release';
const triple = hostTriple();

console.log(`building openfan-service (${profile}) for ${triple}`);
execFileSync(
  'cargo',
  ['build', '-p', 'of-service', ...(profile === 'release' ? ['--release'] : [])],
  { cwd: root, stdio: 'inherit' },
);

const built = join(root, 'target', profile, 'openfan-service.exe');
const staged = join(root, 'src-tauri', 'binaries', `openfan-service-${triple}.exe`);

mkdirSync(dirname(staged), { recursive: true });
copyFileSync(built, staged);
console.log(`staged ${staged}`);
