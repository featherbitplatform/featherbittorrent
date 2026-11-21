// Keep the checked-in generated types honest.
//
// The OpenAPI document fbt-api emits declares the operationId `set_limits` on
// both `PUT /api/v1/session/limits` and `PUT /api/v1/torrents/{id}/limits`, so
// openapi-typescript names one interface member twice and TypeScript rejects
// the file. Rather than transform the document, which Task 3 pins byte for
// byte, mark the generated file with `@ts-nocheck` while the generated types
// do not typecheck, and name the collision in the comment. The marker is
// removed as soon as the generated file compiles, which is what a fix upstream
// looks like here.
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

// Where this file lives, and the project above it, found the way `check-bundle-size.mjs` finds the
// folder next to it. A URL's `pathname` is not a path: it keeps percent-encoding
// ('/Users/Some%20Name/web') and carries no drive letter on Windows, and a path handed to `tsc` or
// used as a working directory has to be a real path in both of those ways.
const here = dirname(fileURLToPath(import.meta.url));
const project = join(here, '..');
const target = join(project, 'src/api/schema.ts');

// This file is an ES module, so the CommonJS resolver exists only through `createRequire`. It is
// how the typecheck below finds the compiler to run, package-relative, rather than by PATH.
const require = createRequire(import.meta.url);

const header = readFileSync(target, 'utf8');
const banner = header.split('\n').slice(0, 4).join('\n');

const MARKER = '// @ts-nocheck';
const EXPLANATION = [
  MARKER,
  '// The generated OpenAPI document declares the same operationId on more',
  '// than one operation, which makes openapi-typescript emit a member twice',
  '// and TypeScript reject the file. Upstream owns the fix; this marker',
  '// disappears once the generated types typecheck.',
].join('\n');

const withoutMarker = header.includes(MARKER)
  ? header
      .split('\n')
      .filter(
        (line, index) =>
          index < 4 || (line !== MARKER && !line.startsWith('// The generated OpenAPI document declares')),
      )
      .join('\n')
  : header;

function typecheck() {
  // The compiler runs from its own package, started by the very node process running this script,
  // rather than through `npx`: `npx` is an executable on Unix and a `.cmd` shim on Windows, and
  // starting a `.cmd` needs a shell - the thing this script must not ask for. The CLI script's path
  // is the one the `typescript` package declares in its `bin` entry (`bin/tsc`), found through
  // Node's own package resolution rather than by name on PATH. With no shell there is nothing to
  // split arguments on, so naming that absolute path is safe, and the project keeps being named
  // the same way it always was: by the config file inside it, found from the working directory,
  // with `-p .` and `--pretty false` as unchanged as the diagnostics read out of the pipes.
  const tsc = require.resolve('typescript/bin/tsc');
  const run = spawnSync(process.execPath, [tsc, '-p', '.', '--pretty', 'false'], {
    cwd: project,
    encoding: 'utf8',
  });
  const diagnostics = `${run.stdout ?? ''}${run.stderr ?? ''}`;
  return {
    clean: run.status === 0,
    namesGeneratedFile: diagnostics.includes('src/api/schema.ts'),
    diagnostics,
  };
}

function main() {
  const before = typecheck();

  // The generated file is the only thing in the project @ts-nocheck can excuse,
  // so a failure that names it is the collision case.
  if (before.namesGeneratedFile) {
    writeFileSync(target, banner + '\n' + EXPLANATION + '\n' + withoutMarker.slice(banner.length));
    const after = typecheck();
    if (after.namesGeneratedFile) {
      process.stderr.write(
        'the generated types do not typecheck even with @ts-nocheck on that file:\n' + after.diagnostics,
      );
      return 1;
    }
    process.stdout.write('src/api/schema.ts: marked @ts-nocheck, the OpenAPI document reuses an operationId\n');
    return 0;
  }

  if (header.includes(MARKER)) {
    writeFileSync(target, withoutMarker);
    process.stdout.write('src/api/schema.ts: removed @ts-nocheck, the generated types typecheck\n');
    if (!before.clean) {
      process.stderr.write(before.diagnostics);
      return 1;
    }
    return 0;
  }

  if (!before.clean) {
    process.stderr.write(before.diagnostics);
    return 1;
  }
  return 0;
}

process.exitCode = main();
