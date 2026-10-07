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

// ---------------------------------------------------------------------------
// Privacy pass (P-HYG.C1). Every identifier below is made up.
// ---------------------------------------------------------------------------

/** Runs the janitor with arbitrary args; returns { status, output }. */
function runArgs(root, ...args) {
  try {
    const output = execFileSync(process.execPath, [JANITOR, ...args, '--root', root], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    return { status: 0, output };
  } catch (e) {
    return { status: e.status, output: `${e.stdout ?? ''}${e.stderr ?? ''}` };
  }
}

function git(root, ...args) {
  execFileSync('git', ['-c', 'init.defaultBranch=main', ...args], { cwd: root, stdio: 'ignore' });
}

const LEAKS = {
  'docs/serial_kw.md': ['intro\n', 'serial: XQ7700112233\n', 'serial'],
  'notes/samsung.txt': ['a\n', 'R9ZZ0000000\n', 'serial'],
  'notes/adb.txt': ['List of devices attached\n', 'XQ7700112233\tdevice\n', 'serial'],
  'notes/udid.md': ['x\n', 'UDID 00008120-000A1B2C3D4E5F60\n', 'udid'],
  'notes/users.md': ['x\n', 'see /Users/jdoe/code/app\n', 'home-path'],
  'notes/home.sh': ['#!/bin/sh\n', 'cd /home/jdoe/app\n', 'home-path'],
  'notes/win.md': ['x\n', 'C:\\Users\\jdoe\\app\n', 'home-path'],
  'notes/dash.md': ['x\n', 'scratch at -private-tmp-claude-501-Users-jdoe-code-app\n', 'home-path'],
  'notes/email.ts': ['// x\n', 'const who = "jdoe@corp.invalid";\n', 'email'],
  'notes/lan.yml': ['a: 1\n', 'host: 192.168.1.23\n', 'local-network'],
  'notes/ten.md': ['x\n', 'dev box at 10.0.0.7\n', 'local-network'],
  'notes/mac.md': ['x\n', 'wifi a4:5e:60:00:11:22\n', 'local-network'],
};
const LEAK_VALUES = [
  'XQ7700112233', 'R9ZZ0000000', '00008120-000A1B2C3D4E5F60', 'jdoe', 'corp.invalid',
  '192.168.1.23', '10.0.0.7', 'a4:5e:60:00:11:22',
];

test('privacy: each class is flagged by file:line and class, value never printed', () => {
  const files = { 'freehike-core/LOOPLOG.md': '# log\n' };
  for (const [rel, [first, second]] of Object.entries(LEAKS)) files[rel] = first + second;
  const root = fixture(files);
  const check = run(root, '--check');
  assert.equal(check.status, 1, check.output);
  for (const [rel, [, , cls]] of Object.entries(LEAKS)) {
    assert.match(check.output, new RegExp(`${rel.replace(/\./g, '\\.')}:2: privacy \\(${cls}\\)`), rel);
  }
  for (const v of LEAK_VALUES) assert.ok(!check.output.includes(v), `value leaked into output: class of ${v.length}-char value`);
});

test('privacy: a serial split across a line wrap is caught', () => {
  const root = fixture({
    'freehike-core/LOOPLOG.md': '# log\n\n- Device: Phone X, Android 16, USB adb, serial\n  `XQ7700112233`.\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 1, check.output);
  assert.match(check.output, /freehike-core\/LOOPLOG\.md:3: privacy \(serial\)/);
});

test('privacy: placeholders, image names, loopback and look-alikes pass', () => {
  const root = fixture({
    'freehike-core/LOOPLOG.md': '# log\n',
    'docs/ok.md': [
      'Placeholders: /Users/<you>/code, /Users/$USER/code, ~/code, /Users/Shared/data, /home/<you>',
      'Images: icon@2x.png, AppIcon@3x.png',
      'Loopback: http://127.0.0.1:5173 and 0.0.0.0:8080',
      'Allowed mail: someone@example.com, bot@users.noreply.github.com',
      'REGENERATED is a word; commit 0123456789abcdef0123456789abcdef01234567',
      'the serial number field; the serial followed by prose; serialization; serial format v7',
      'UUID 123e4567-e89b-12d3-a456-426614174000; version 10.2.3; Samsung SM-S918B, Android 16',
      '',
    ].join('\n'),
    'ios/Contents.json': '{ "filename" : "AppIcon@2x.png" }\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 0, check.output);
  assert.doesNotMatch(check.output, /privacy/);
});

test('privacy: Markdown and docs/ are scanned, generated trees are not', () => {
  const root = fixture({
    'freehike-core/LOOPLOG.md': '# log\n',
    'freehike-core/ffi/bindings/freehike.swift': '// built in /Users/jdoe/code\n',
    'node_modules/pkg/README.md': 'author jdoe@corp.invalid\n',
    'docs/guide.md': '# guide\nfrom /Users/jdoe/code\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 1, check.output);
  assert.match(check.output, /docs\/guide\.md:2: privacy \(home-path\)/);
  assert.doesNotMatch(check.output, /bindings|node_modules/);
});

test('privacy --staged: reads the index, not the disk (both directions)', () => {
  const root = fixture({ 'freehike-core/LOOPLOG.md': '# log\n', 'notes.md': '# clean\n' });
  git(root, 'init', '-q');
  git(root, 'add', '-A');

  // Staged clean, leak only on disk: the hook passes, a tree check fails.
  fs.writeFileSync(path.join(root, 'notes.md'), '# clean\nserial XQ7700112233\n');
  let staged = runArgs(root, '--check', '--staged');
  assert.equal(staged.status, 0, staged.output);
  let disk = run(root, '--check');
  assert.equal(disk.status, 1, disk.output);
  assert.match(disk.output, /notes\.md:2: privacy \(serial\)/);

  // Leak staged, disk fixed afterwards: the hook still refuses.
  git(root, 'add', 'notes.md');
  fs.writeFileSync(path.join(root, 'notes.md'), '# clean\n');
  staged = runArgs(root, '--check', '--staged');
  assert.equal(staged.status, 1, staged.output);
  assert.match(staged.output, /notes\.md:2: privacy \(serial\)/);
  disk = run(root, '--check');
  assert.equal(disk.status, 0, disk.output);
});

test('privacy --staged: refused outside a git work tree', () => {
  const root = fixture({ 'freehike-core/LOOPLOG.md': '# log\n' });
  const staged = runArgs(root, '--check', '--staged');
  assert.equal(staged.status, 2, staged.output);
  assert.match(staged.output, /--staged needs a git work tree/);
});

test('privacy: a serial a few words after the keyword is caught; port/baud/format prose passes', () => {
  const root = fixture({
    'freehike-core/LOOPLOG.md': '# log\n\nthe phone serial is XQ77A0112233\n',
    'docs/ok.md': 'serial port ttyUSB0\nserial console 115200\nserial format v7\n',
  });
  const check = run(root, '--check');
  assert.equal(check.status, 1, check.output);
  assert.match(check.output, /freehike-core\/LOOPLOG\.md:3: privacy \(serial\)/);
  assert.doesNotMatch(check.output, /docs\/ok\.md/);
  assert.ok(!check.output.includes('XQ77A0112233'));
});
