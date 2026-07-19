#!/usr/bin/env node
// SPDX-License-Identifier: Apache-2.0
//
// tracker-janitor.mjs — rules enforcement + debt/bug tracking pipeline.
//
// Modes:
//   --check   read-only; non-zero exit on violation (pre-commit hook + CI)
//   --fix     regenerate TRACKER.md in full from the code scan
//   --root D  operate on D instead of the repo root (used by the test suite)
//
// Authority model (settled — see docs/tracker_tags.md):
//   - ARCHITECTURE.md owns rules (fenced rule-id/forbidden-pattern/paths blocks)
//   - inline DEBT/BUG/RULE-EXEMPT tags in code are the source of truth for
//     open items
//   - TRACKER.md is GENERATED, machine-owned, never authoritative
//   - freehike-core/LOOPLOG.md is the only graveyard ("closes D###/B###")
//
// Node built-ins only. Boring by design: file walks + regex.

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/** File extensions scanned for tags and forbidden patterns. Markdown is
 *  deliberately excluded: tags are code comments, and scanning docs would
 *  turn every spec example into a live tag. */
const SCAN_EXTENSIONS = new Set([
  '.rs', '.swift', '.kt', '.ts', '.tsx', '.js', '.mjs', '.sh', '.py',
  '.toml', '.yml', '.yaml', '.gradle',
  // Lockfiles carry no comments/tags but ARE subject to forbidden-pattern
  // rules (P5a/P6a match `name = "..."` entries → transitive deps caught).
  '.lock',
]);

/** Path prefixes (relative, POSIX separators) skipped entirely. */
const IGNORE_PREFIXES = [
  '.git/',
  'node_modules/',
  'dist/',
  'docs/',
  'freehike-core/target/',
  'freehike-core/ffi/bindings/',            // generated UniFFI (canonical)
  'android/app/src/main/java/uniffi/',      // generated UniFFI (vendored)
  'ios/App/App/FreeHikeFFI/',               // generated UniFFI (vendored)
  'android/app/build/', 'android/build/', 'android/.gradle/',
  'ios/App/Pods/',
  'public/glyphs/',
  'offline_sandbox/',
  'scripts/tracker-janitor',                // self (this file + its tests)
  'src/valhalla.js',                        // generated emscripten bundle
];

/** platform name → tree prefix, for the cross-platform mirror check. */
const PLATFORM_TREES = {
  ios: 'ios/',
  android: 'android/',
  web: 'src/',
  core: 'freehike-core/',
};

const SEVERITIES = new Set(['blocker', 'major', 'minor']);

// Tag grammar. The separator is an em dash (—) or a double hyphen (--).
const SEP = String.raw`\s+(?:—|--)\s+`;
const COMMENT = String.raw`(?:\/\/|#)`;
const TAG_HINT = new RegExp(`${COMMENT}.*\\b(DEBT|BUG|RULE-EXEMPT)\\(`);
const DEBT_RE = new RegExp(
  `${COMMENT}\\s*DEBT\\((D\\d{3,})\\):\\s*(.+?)${SEP}platforms:\\s*([a-z, ]+)\\s*$`
);
const BUG_RE = new RegExp(
  `${COMMENT}\\s*BUG\\((B\\d{3,})\\):\\s*(.+?)${SEP}severity:\\s*(\\w+)${SEP}repro:\\s*(.+)\\s*$`
);
const EXEMPT_RE = new RegExp(
  `${COMMENT}\\s*RULE-EXEMPT\\(([PR]\\d+[a-z]?)\\):\\s*(\\S.*)\\s*$`
);

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

function walk(root, rel = '', out = []) {
  const abs = path.join(root, rel);
  for (const entry of fs.readdirSync(abs, { withFileTypes: true })) {
    const childRel = rel === '' ? entry.name : `${rel}/${entry.name}`;
    if (IGNORE_PREFIXES.some((p) => childRel === p.replace(/\/$/, '') || `${childRel}/`.startsWith(p) || childRel.startsWith(p))) {
      continue;
    }
    if (entry.isDirectory()) {
      walk(root, childRel, out);
    } else if (entry.isFile() && SCAN_EXTENSIONS.has(path.extname(entry.name))) {
      out.push(childRel);
    }
  }
  return out;
}

/** Tiny glob → regex: `**` crosses directories, `*` stays within one. */
function globToRegex(glob) {
  const DOUBLE = '\u0001';
  const esc = glob
    .replace(/[.+^${}()|[\]\\]/g, '\\$&')
    .replace(/\*\*/g, DOUBLE)
    .replace(/\*/g, '[^/]*')
    .replace(/\u0001/g, '.*');
  return new RegExp(`^${esc}$`);
}

// ---------------------------------------------------------------------------
// Scanning: tags
// ---------------------------------------------------------------------------

function scanTags(root, files, errors) {
  const debts = new Map();    // id -> { description, platforms, sites: [{file,line}] }
  const bugs = new Map();     // id -> { description, severity, repro, sites }
  const exemptions = [];      // { rule, file, line, justification }

  for (const file of files) {
    const lines = fs.readFileSync(path.join(root, file), 'utf8').split('\n');
    lines.forEach((text, i) => {
      const line = i + 1;
      if (!TAG_HINT.test(text)) return;

      let m;
      if ((m = DEBT_RE.exec(text))) {
        const [, id, description, platformsRaw] = m;
        const platforms = platformsRaw.split(',').map((p) => p.trim()).filter(Boolean);
        const bad = platforms.filter((p) => !(p in PLATFORM_TREES));
        if (bad.length || platforms.length === 0) {
          errors.push(`${file}:${line}: DEBT(${id}) has invalid platforms list '${platformsRaw.trim()}' (allowed: ${Object.keys(PLATFORM_TREES).join(',')})`);
          return;
        }
        const existing = debts.get(id);
        if (existing) {
          if (existing.description !== description.trim() || existing.platforms.join(',') !== platforms.join(',')) {
            errors.push(`${file}:${line}: DEBT(${id}) conflicts with ${existing.sites[0].file}:${existing.sites[0].line} (description/platforms must match at every site)`);
            return;
          }
          existing.sites.push({ file, line });
        } else {
          debts.set(id, { description: description.trim(), platforms, sites: [{ file, line }] });
        }
      } else if ((m = BUG_RE.exec(text))) {
        const [, id, description, severity, repro] = m;
        if (!SEVERITIES.has(severity)) {
          errors.push(`${file}:${line}: BUG(${id}) has invalid severity '${severity}' (allowed: ${[...SEVERITIES].join('|')})`);
          return;
        }
        const existing = bugs.get(id);
        if (existing) {
          if (existing.description !== description.trim() || existing.severity !== severity) {
            errors.push(`${file}:${line}: BUG(${id}) conflicts with ${existing.sites[0].file}:${existing.sites[0].line} (description/severity must match at every site)`);
            return;
          }
          existing.sites.push({ file, line });
        } else {
          bugs.set(id, { description: description.trim(), severity, repro: repro.trim(), sites: [{ file, line }] });
        }
      } else if ((m = EXEMPT_RE.exec(text))) {
        exemptions.push({ rule: m[1], file, line, justification: m[2].trim() });
      } else {
        errors.push(`${file}:${line}: malformed tag — does not parse as DEBT/BUG/RULE-EXEMPT (see docs/tracker_tags.md): ${text.trim()}`);
      }
    });
  }
  return { debts, bugs, exemptions };
}

function checkMirrors(debts, errors) {
  for (const [id, debt] of debts) {
    for (const platform of debt.platforms) {
      const tree = PLATFORM_TREES[platform];
      if (!debt.sites.some((s) => s.file.startsWith(tree))) {
        errors.push(
          `DEBT(${id}) declares platforms '${debt.platforms.join(',')}' but has no tag site under ${tree} ` +
          `(sites: ${debt.sites.map((s) => `${s.file}:${s.line}`).join(', ')}) — a cross-platform debt is ONE id tagged in EACH declared tree`
        );
      }
    }
  }
}

// ---------------------------------------------------------------------------
// Scanning: mechanical rules from ARCHITECTURE.md
// ---------------------------------------------------------------------------

function parseRules(root, errors) {
  const archPath = path.join(root, 'ARCHITECTURE.md');
  if (!fs.existsSync(archPath)) return [];
  const rules = [];
  let current = null;
  for (const raw of fs.readFileSync(archPath, 'utf8').split('\n')) {
    const line = raw.trim();
    let m;
    if ((m = /^rule-id:\s*(\S+)$/.exec(line))) {
      current = { id: m[1], pattern: null, paths: [] };
      rules.push(current);
    } else if (current && (m = /^forbidden-pattern:\s*(.+)$/.exec(line))) {
      try {
        current.pattern = new RegExp(m[1]);
      } catch (e) {
        errors.push(`ARCHITECTURE.md rule ${current.id}: forbidden-pattern is not a valid regex: ${e.message}`);
      }
    } else if (current && (m = /^paths:\s*(.+)$/.exec(line))) {
      current.paths = m[1].split(',').map((g) => globToRegex(g.trim()));
    } else if (line === '' || line.startsWith('```')) {
      if (current && (!current.pattern || current.paths.length === 0) && line.startsWith('```')) {
        // Block closed while incomplete → declared but unenforceable.
        if (!current.pattern) errors.push(`ARCHITECTURE.md rule ${current.id}: missing forbidden-pattern`);
        if (current.paths.length === 0) errors.push(`ARCHITECTURE.md rule ${current.id}: missing paths`);
      }
      if (line.startsWith('```')) current = null;
    }
  }
  return rules.filter((r) => r.pattern && r.paths.length > 0);
}

function checkForbiddenPatterns(root, files, rules, exemptions, errors) {
  if (rules.length === 0) return;
  const exemptAt = new Set(exemptions.map((e) => `${e.rule}@${e.file}:${e.line}`));
  for (const file of files) {
    const applicable = rules.filter((r) => r.paths.some((re) => re.test(file)));
    if (applicable.length === 0) continue;
    const lines = fs.readFileSync(path.join(root, file), 'utf8').split('\n');
    lines.forEach((text, i) => {
      const line = i + 1;
      for (const rule of applicable) {
        if (!rule.pattern.test(text)) continue;
        if (EXEMPT_RE.test(text)) continue; // the exemption tag itself
        // Suppressed by RULE-EXEMPT(rule) on the same line or the line above.
        const suppressed =
          exemptAt.has(`${rule.id}@${file}:${line}`) || exemptAt.has(`${rule.id}@${file}:${line - 1}`);
        if (!suppressed) {
          errors.push(
            `${file}:${line}: forbidden pattern for rule ${rule.id} (${rule.pattern.source}) — ` +
            `fix it, or sanction it with a RULE-EXEMPT(${rule.id}) tag on the line above`
          );
        }
      }
    });
  }
}

// ---------------------------------------------------------------------------
// TRACKER.md generation + drift checks
// ---------------------------------------------------------------------------

function renderTracker({ debts, bugs, exemptions }) {
  const lines = [
    '# TRACKER.md — GENERATED — do not edit',
    '',
    '> Machine-owned view generated by `scripts/tracker-janitor.mjs --fix`.',
    '> The inline tags in code are authoritative (docs/tracker_tags.md);',
    '> fixed items are buried in `freehike-core/LOOPLOG.md` via `closes D###`/`closes B###`.',
    '',
    '## Open Debt',
    '',
  ];
  const debtIds = [...debts.keys()].sort();
  if (debtIds.length === 0) lines.push('_none_');
  for (const id of debtIds) {
    const d = debts.get(id);
    lines.push(`- **${id}** — ${d.description} — platforms: ${d.platforms.join(',')}`);
    for (const s of [...d.sites].sort((a, b) => a.file.localeCompare(b.file) || a.line - b.line)) {
      lines.push(`  - ${s.file}:${s.line}`);
    }
  }
  lines.push('', '## Open Bugs', '');
  const bugIds = [...bugs.keys()].sort();
  if (bugIds.length === 0) lines.push('_none_');
  for (const id of bugIds) {
    const b = bugs.get(id);
    lines.push(`- **${id}** — [${b.severity}] ${b.description} — repro: ${b.repro}`);
    for (const s of [...b.sites].sort((a, b2) => a.file.localeCompare(b2.file) || a.line - b2.line)) {
      lines.push(`  - ${s.file}:${s.line}`);
    }
  }
  lines.push('', '## Rule Exemptions', '');
  const sorted = [...exemptions].sort(
    (a, b) => a.rule.localeCompare(b.rule) || a.file.localeCompare(b.file) || a.line - b.line
  );
  if (sorted.length === 0) lines.push('_none_');
  for (const e of sorted) {
    lines.push(`- **${e.rule}** — ${e.file}:${e.line} — ${e.justification}`);
  }
  lines.push('');
  return lines.join('\n');
}

function checkTrackerDrift(root, scan, warnings) {
  const trackerPath = path.join(root, 'TRACKER.md');
  const regenerated = renderTracker(scan);
  if (!fs.existsSync(trackerPath)) {
    warnings.push('TRACKER.md does not exist yet — run `node scripts/tracker-janitor.mjs --fix`');
    return;
  }
  const committed = fs.readFileSync(trackerPath, 'utf8');
  if (committed !== regenerated) {
    warnings.push('TRACKER.md is stale relative to the code scan — run `node scripts/tracker-janitor.mjs --fix`');
  }

  // Resolved-but-not-buried: an ID present in the committed TRACKER but no
  // longer tagged anywhere AND not closed in LOOPLOG.
  const trackedIds = [...committed.matchAll(/\*\*([DB]\d{3,})\*\*/g)].map((m) => m[1]);
  const liveIds = new Set([...scan.debts.keys(), ...scan.bugs.keys()]);
  const looplogPath = path.join(root, 'freehike-core/LOOPLOG.md');
  const looplog = fs.existsSync(looplogPath) ? fs.readFileSync(looplogPath, 'utf8') : '';
  for (const id of trackedIds) {
    if (!liveIds.has(id) && !new RegExp(`closes\\s+${id}\\b`).test(looplog)) {
      warnings.push(
        `${id} is in TRACKER.md but no longer tagged in code and has no 'closes ${id}' in LOOPLOG.md — ` +
        `resolved but not buried: append the LOOPLOG kill entry, then --fix`
      );
    }
  }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

function main() {
  const args = process.argv.slice(2);
  const mode = args.includes('--fix') ? 'fix' : args.includes('--check') ? 'check' : null;
  const rootIdx = args.indexOf('--root');
  const root = rootIdx >= 0 ? path.resolve(args[rootIdx + 1]) : path.resolve(path.dirname(new URL(import.meta.url).pathname), '..');
  if (!mode) {
    console.error('usage: tracker-janitor.mjs --check|--fix [--root <dir>]');
    process.exit(2);
  }

  const errors = [];
  const warnings = [];
  const files = walk(root).sort();
  const scan = scanTags(root, files, errors);
  checkMirrors(scan.debts, errors);
  const rules = parseRules(root, errors);
  checkForbiddenPatterns(root, files, rules, scan.exemptions, errors);

  if (mode === 'fix') {
    fs.writeFileSync(path.join(root, 'TRACKER.md'), renderTracker(scan));
    console.log(
      `TRACKER.md regenerated: ${scan.debts.size} debt, ${scan.bugs.size} bug, ${scan.exemptions.length} exemption item(s).`
    );
  } else {
    checkTrackerDrift(root, scan, warnings);
  }

  for (const w of warnings) console.log(`WARN: ${w}`);
  for (const e of errors) console.error(`ERROR: ${e}`);
  if (errors.length > 0) {
    console.error(`\ntracker-janitor: ${errors.length} violation(s).`);
    process.exit(1);
  }
  if (mode === 'check') {
    console.log(`tracker-janitor: clean (${scan.debts.size} debt, ${scan.bugs.size} bug, ${scan.exemptions.length} exemption item(s), ${warnings.length} warning(s)).`);
  }
}

main();
