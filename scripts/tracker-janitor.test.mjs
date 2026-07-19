// SPDX-License-Identifier: Apache-2.0
//
// tracker-janitor.test.mjs — `node --test scripts/tracker-janitor.test.mjs`
//
// Real-filesystem fixtures (fresh temp dir per test, actual files, actual
// child-process invocations of the janitor) — an unverified verifier is
// worthless. Covers: happy path, mirror-check failure, forbidden-pattern
// hit, exemption suppression, resolved-but-not-buried warning, malformed
// tag rejection.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const JANITOR = path.join(path.dirname(fileURLToPath(import.meta.url)), 'tracker-janitor.mjs');

/** Creates a throwaway repo-shaped fixture directory. `files` maps relative
 *  path → content; parent dirs are created as needed. */
function fixture(files) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'janitor-fixture-'));
  for (const [rel, content] of Object.entries(files)) {
    const abs = path.join(root, rel);
    fs.mkdirSync(path.dirname(abs), { recursive: true });
    fs.writeFileSync(abs, content);
  }
  return root;
}

/** Runs the janitor; returns { status, output } without throwing. */
function run(root, mode) {
  try {
    const output = execFileSync(process.execPath, [JANITOR, mode, '--root', root], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    return { status: 0, output };
  } catch (e) {
    return { status: e.status, output: `${e.stdout ?? ''}${e.stderr ?? ''}` };
  }
}

const ARCH_WITH_RULE = [
  '# Fixture architecture',
  '',
  '**P8 — Frontend discipline.** Theme switching via setPaintProperty, never setStyle teardown.',
  '',
  '```',
  'rule-id: P8a',
  'forbidden-pattern: setStyle\\(',
  'paths: src/**',
  '```',
  '',
].join('\n');

// ---------------------------------------------------------------------------

test('happy path: valid tags scan clean, --fix generates deterministic TRACKER.md, --check accepts it', () => {
  const root = fixture({
    'src/map.ts': '// DEBT(D001): shared debt across web and android — platforms: web,android\nexport const x = 1;\n',
    'android/app/Job.kt': '// DEBT(D001): shared debt across web and android — platforms: web,android\nval x = 1\n',
    'freehike-core/compiler/src/engine.rs': '// BUG(B001): checkpoint nit — severity: minor — repro: LOOPLOG P4.C2\nfn main() {}\n',
    'freehike-core/LOOPLOG.md': '# log\n',
    'ARCHITECTURE.md': ARCH_WITH_RULE,
  });

  const fix = run(root, '--fix');
  assert.equal(fix.status, 0, fix.output);
  const tracker = fs.readFileSync(path.join(root, 'TRACKER.md'), 'utf8');
  assert.match(tracker, /GENERATED — do not edit/);
  assert.match(tracker, /\*\*D001\*\* — shared debt across web and android — platforms: web,android/);
  assert.match(tracker, /src\/map\.ts:1/);
  assert.match(tracker, /android\/app\/Job\.kt:1/);
  assert.match(tracker, /\*\*B001\*\* — \[minor\] checkpoint nit — repro: LOOPLOG P4\.C2/);

  // Determinism: a second --fix is byte-identical.
  run(root, '--fix');
  assert.equal(fs.readFileSync(path.join(root, 'TRACKER.md'), 'utf8'), tracker);

  const check = run(root, '--check');
  assert.equal(check.status, 0, check.output);
  assert.doesNotMatch(check.output, /WARN/);
});

test('mirror-check failure: platforms declared but one tree untagged fails --check', () => {
  const root = fixture({
    'android/app/Job.kt': '// DEBT(D002): ios+android debt tagged only on android — platforms: ios,android\nval x = 1\n',
    'freehike-core/LOOPLOG.md': '# log\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 1, check.output);
  assert.match(check.output, /DEBT\(D002\).*no tag site under ios\//);
});

test('forbidden-pattern hit without exemption fails --check', () => {
  const root = fixture({
    'ARCHITECTURE.md': ARCH_WITH_RULE,
    'src/theme.ts': 'export function swap(map) {\n  map.setStyle(next);\n}\n',
    'freehike-core/LOOPLOG.md': '# log\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 1, check.output);
  assert.match(check.output, /src\/theme\.ts:2: forbidden pattern for rule P8a/);
});

test('RULE-EXEMPT on the preceding line suppresses the match and is listed in TRACKER.md', () => {
  const root = fixture({
    'ARCHITECTURE.md': ARCH_WITH_RULE,
    'src/theme.ts':
      '// RULE-EXEMPT(P8a): initial style load — the one sanctioned setStyle call\nmap.setStyle(base);\n',
    'freehike-core/LOOPLOG.md': '# log\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 0, check.output);

  run(root, '--fix');
  const tracker = fs.readFileSync(path.join(root, 'TRACKER.md'), 'utf8');
  assert.match(tracker, /\*\*P8a\*\* — src\/theme\.ts:1 — initial style load/);
});

test('resolved-but-not-buried: tracked ID gone from code warns until LOOPLOG closes it', () => {
  const root = fixture({
    'src/map.ts': '// DEBT(D010): temporary debt — platforms: web\nexport const x = 1;\n',
    'freehike-core/LOOPLOG.md': '# log\n',
  });
  run(root, '--fix');

  // Fix the debt: remove the tag but do NOT bury it in LOOPLOG.
  fs.writeFileSync(path.join(root, 'src/map.ts'), 'export const x = 1;\n');
  let check = run(root, '--check');
  assert.equal(check.status, 0, check.output); // warnings never fail the build
  assert.match(check.output, /WARN: D010 .*resolved but not buried/);

  // Bury it: the warning about D010 disappears (staleness warning remains
  // until --fix, which is exactly the prompt to regenerate).
  fs.appendFileSync(path.join(root, 'freehike-core/LOOPLOG.md'), '\nkill entry: closes D010\n');
  check = run(root, '--check');
  assert.equal(check.status, 0, check.output);
  assert.doesNotMatch(check.output, /resolved but not buried/);
  assert.match(check.output, /WARN: TRACKER\.md is stale/);

  run(root, '--fix');
  check = run(root, '--check');
  assert.doesNotMatch(check.output, /WARN/);
});

test('malformed tags are rejected: bad ID width, missing fields, bad severity', () => {
  const root = fixture({
    'src/a.ts': '// DEBT(D12): id too short — platforms: web\n',
    'src/b.ts': '// BUG(B001): no severity or repro fields\n',
    'src/c.ts': '// BUG(B002): bad severity — severity: catastrophic — repro: none\n',
    'src/d.ts': '// RULE-EXEMPT(P8a):\n',
    'freehike-core/LOOPLOG.md': '# log\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 1, check.output);
  assert.match(check.output, /src\/a\.ts:1: malformed tag/);
  assert.match(check.output, /src\/b\.ts:1: malformed tag/);
  assert.match(check.output, /src\/c\.ts:1: (malformed tag|.*invalid severity)/);
  assert.match(check.output, /src\/d\.ts:1: malformed tag/);
});

test('conflicting descriptions for one ID fail --check', () => {
  const root = fixture({
    'src/a.ts': '// DEBT(D005): description one — platforms: web,core\n',
    'freehike-core/x.rs': '// DEBT(D005): a different description — platforms: web,core\n',
    'freehike-core/LOOPLOG.md': '# log\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 1, check.output);
  assert.match(check.output, /DEBT\(D005\) conflicts with/);
});

test('generated trees and ignore list are not scanned', () => {
  const root = fixture({
    'node_modules/pkg/index.js': '// DEBT(D999): should never be seen — platforms: web\n',
    'android/app/src/main/java/uniffi/freehike/freehike.kt': '// BUG(B999): generated — severity: blocker — repro: n/a\n',
    'public/glyphs/font.sh': '# BUG(bad tag that would be malformed\n',
    'freehike-core/LOOPLOG.md': '# log\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 0, check.output);
  assert.match(check.output, /0 debt, 0 bug/);
});
