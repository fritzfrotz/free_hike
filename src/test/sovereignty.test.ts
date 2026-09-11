// SPDX-License-Identifier: Apache-2.0
/// <reference types="node" />
/**
 * sovereignty.test.ts — P10 (sovereign runtime) proofs at the source level.
 *
 * 1. index.html carries a Content-Security-Policy meta whose value is
 *    exactly `connect-src 'self'` — the runtime guarantee that the WebView
 *    talks to no host but its own origin after ingestion.
 * 2. No non-test source file under src/ contains an absolute http(s) URL.
 *    This mirrors ARCHITECTURE.md rule P10a (janitor-enforced in CI) so the
 *    unit suite fails on the same line the janitor would.
 */
import { describe, expect, it } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';

const ROOT = process.cwd();

function walk(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) walk(full, out);
    else out.push(full);
  }
  return out;
}

describe('P10 sovereign runtime', () => {
  it("index.html declares connect-src 'self' and nothing broader", () => {
    const html = readFileSync(join(ROOT, 'index.html'), 'utf8');
    const meta = /<meta\s+http-equiv="Content-Security-Policy"\s+content="([^"]*)"/i.exec(html);
    expect(meta, 'CSP meta tag missing from index.html').not.toBeNull();
    expect(meta![1]).toBe("connect-src 'self'");
  });

  it('no non-test source under src/ references an absolute http(s) URL', () => {
    const offenders: string[] = [];
    for (const file of walk(join(ROOT, 'src'))) {
      const rel = relative(ROOT, file);
      if (rel.startsWith(join('src', 'test') + '/')) continue;
      if (/\.test\.[jt]sx?$/.test(rel)) continue;
      if (!/\.(ts|tsx|js|css|json)$/.test(rel)) continue;
      readFileSync(file, 'utf8').split('\n').forEach((line, i) => {
        if (/https?:\/\//.test(line)) offenders.push(`${rel}:${i + 1}: ${line.trim()}`);
      });
    }
    expect(offenders, `absolute URLs found:\n${offenders.join('\n')}`).toEqual([]);
  });
});
