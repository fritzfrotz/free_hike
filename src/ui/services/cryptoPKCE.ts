// SPDX-License-Identifier: Apache-2.0
/**
 * cryptoPKCE.ts
 *
 * Standalone PKCE (Proof Key for Code Exchange) primitives for OAuth 2.0
 * public-client flows.  Uses only the native Web Crypto API — zero external
 * dependencies, zero server involvement.
 *
 * Session bridge strategy
 * ───────────────────────
 * The OAuth redirect navigates away from and back to this origin within the
 * same browser tab.  `sessionStorage` is tab-scoped, survives same-tab
 * navigations, and is automatically cleared when the tab is closed — making
 * it the correct storage primitive for the code_verifier bridge.
 *
 * Keys used in sessionStorage:
 *   freehike_pkce_verifier  – the raw unhashed code_verifier string
 *   freehike_pkce_state     – the CSRF state nonce
 */

const VERIFIER_SESSION_KEY = 'freehike_pkce_verifier';
const STATE_SESSION_KEY    = 'freehike_pkce_state';

// ─── Internal helpers ─────────────────────────────────────────────────────────

/**
 * Converts an ArrayBuffer to a base64url-encoded string (RFC 4648 §5).
 * Replaces `+` → `-`, `/` → `_`, strips padding `=`.
 */
function base64urlEncode(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = '';
  for (let i = 0; i < bytes.byteLength; i++) {
    binary += String.fromCharCode(bytes[i]);
  }
  return btoa(binary)
    .replace(/\+/g, '-')
    .replace(/\//g, '_')
    .replace(/=/g, '');
}

// ─── Public API ───────────────────────────────────────────────────────────────

/**
 * Generates a cryptographically random code_verifier string.
 *
 * Uses 32 random bytes → 43-character base64url string, satisfying the
 * RFC 7636 §4.1 requirement of 43–128 characters with sufficient entropy.
 */
export function generateCodeVerifier(): string {
  const randomBytes = new Uint8Array(32);
  crypto.getRandomValues(randomBytes);
  return base64urlEncode(randomBytes.buffer);
}

/**
 * Derives the code_challenge from a verifier using SHA-256.
 *
 * Algorithm: BASE64URL(SHA256(ASCII(code_verifier)))
 * Method parameter sent to provider: `code_challenge_method=S256`
 */
export async function generateCodeChallenge(verifier: string): Promise<string> {
  const encoded = new TextEncoder().encode(verifier);
  const digest  = await crypto.subtle.digest('SHA-256', encoded);
  return base64urlEncode(digest);
}

// ─── sessionStorage bridge ────────────────────────────────────────────────────

/** Persists the code_verifier across the OAuth redirect lifecycle. */
export function storeVerifier(verifier: string): void {
  sessionStorage.setItem(VERIFIER_SESSION_KEY, verifier);
}

/**
 * Retrieves and immediately removes the code_verifier from sessionStorage.
 * Returns `null` if the verifier is absent (e.g. the user navigated directly
 * to the callback URL without initiating a real auth flow).
 */
export function retrieveAndClearVerifier(): string | null {
  const value = sessionStorage.getItem(VERIFIER_SESSION_KEY);
  sessionStorage.removeItem(VERIFIER_SESSION_KEY);
  return value;
}

/** Persists the CSRF state nonce across the OAuth redirect lifecycle. */
export function storeState(state: string): void {
  sessionStorage.setItem(STATE_SESSION_KEY, state);
}

/**
 * Retrieves and immediately removes the CSRF state nonce from sessionStorage.
 * Returns `null` if absent.
 */
export function retrieveAndClearState(): string | null {
  const value = sessionStorage.getItem(STATE_SESSION_KEY);
  sessionStorage.removeItem(STATE_SESSION_KEY);
  return value;
}
