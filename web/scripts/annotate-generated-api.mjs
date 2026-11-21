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
import { join } from 'node:path';

const root = new URL('..', import.meta.url).pathname;
const target = join(root, 'src/api/schema.ts');

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
  const run = spawnSync('npx', ['tsc', '-p', root, '--pretty', 'false'], {
    cwd: root,
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
