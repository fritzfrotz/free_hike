/**
 * DropboxSync.ts
 *
 * Full Dropbox OAuth 2.0 PKCE provider bridge.
 *
 * Auth flow:
 *   buildDropboxAuthUrl()      → redirect user to Dropbox consent screen
 *   exchangeDropboxCode(code)  → swap auth code for short-lived access token
 *                                + long-lived refresh token (token_access_type=offline)
 *   syncToDropbox(gpx, meta)   → overwrite two files in /Apps/AntigravityApp/
 *   disconnectDropbox()        → wipe localStorage token
 *
 * Upload model:
 *   Dropbox uses a flat path model — no folder ID step required.
 *   The /Apps/AntigravityApp/ directory is created implicitly on first upload
 *   because the app was registered with the Files.content.write permission and
 *   the "App folder" access type, which sandboxes it automatically.
 *
 * Token lifecycle:
 *   getDropboxValidToken() checks expiresAt before every upload call.
 *   If the token expires within 60 s it silently refreshes via the stored
 *   refresh_token before returning the access token string.
 */

import type { OAuthTokenRecord } from '../../../shared/types';
import {
  generateCodeVerifier,
  generateCodeChallenge,
  storeVerifier,
  storeState,
  retrieveAndClearVerifier,
} from '../cryptoPKCE';

// ─── Configuration ────────────────────────────────────────────────────────────
// Replace with your App Key from dropbox.com/developers/apps.

const DROPBOX_APP_KEY      = 'YOUR_DROPBOX_APP_KEY';
const DROPBOX_AUTH_URL     = 'https://www.dropbox.com/oauth2/authorize';
const DROPBOX_TOKEN_URL    = 'https://api.dropboxapi.com/oauth2/token';
const DROPBOX_USERINFO_URL = 'https://api.dropboxapi.com/2/users/get_current_account';
const DROPBOX_UPLOAD_URL   = 'https://content.dropboxapi.com/2/files/upload';
const DROPBOX_FOLDER_PATH  = '/Apps/AntigravityApp';
const DROPBOX_TOKEN_KEY    = 'antigravity_dropbox_token';

// Dropbox short-lived tokens are 4 hours by default when not specified.
const DROPBOX_DEFAULT_TTL_MS = 4 * 60 * 60 * 1_000;

// ─── Internal type helpers ────────────────────────────────────────────────────

interface TokenResp   {
  access_token: string;
  refresh_token?: string;
  expires_in?: number;
  scope?: string;
}
interface RefreshResp {
  access_token: string;
  expires_in?: number;
  scope?: string;
}
interface AccountResp { email: string }

// ─── localStorage helpers ─────────────────────────────────────────────────────

/** Returns the stored token record or null if the user has never connected. */
export function loadDropboxTokenRecord(): OAuthTokenRecord | null {
  const raw = localStorage.getItem(DROPBOX_TOKEN_KEY);
  if (!raw) return null;
  try   { return JSON.parse(raw) as OAuthTokenRecord; }
  catch { return null; }
}

function saveDropboxToken(record: OAuthTokenRecord): void {
  localStorage.setItem(DROPBOX_TOKEN_KEY, JSON.stringify(record));
}

/** Removes the token from localStorage. Call on user-initiated disconnect. */
export function disconnectDropbox(): void {
  localStorage.removeItem(DROPBOX_TOKEN_KEY);
}

// ─── Token lifecycle ──────────────────────────────────────────────────────────

async function refreshDropboxToken(record: OAuthTokenRecord): Promise<OAuthTokenRecord> {
  if (!record.refreshToken) {
    throw new Error('[Dropbox] No refresh token stored — user must re-authenticate.');
  }

  const body = new URLSearchParams({
    client_id:     DROPBOX_APP_KEY,
    grant_type:    'refresh_token',
    refresh_token: record.refreshToken,
  });

  const res = await fetch(DROPBOX_TOKEN_URL, {
    method:  'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body:    body.toString(),
  });

  if (!res.ok) {
    throw new Error(`[Dropbox] Token refresh failed: HTTP ${res.status}`);
  }

  const data = await res.json() as RefreshResp;
  const updated: OAuthTokenRecord = {
    ...record,
    accessToken: data.access_token,
    expiresAt:   Date.now() + (data.expires_in ?? DROPBOX_DEFAULT_TTL_MS / 1_000) * 1_000,
    scope:       data.scope ?? record.scope,
  };

  saveDropboxToken(updated);
  return updated;
}

/**
 * Returns a valid access token, auto-refreshing if within 60 s of expiry.
 * Throws if no token exists — caller must ensure the user is connected first.
 */
async function getDropboxValidToken(): Promise<string> {
  const record = loadDropboxTokenRecord();
  if (!record) {
    throw new Error('[Dropbox] No token found. Connect Dropbox first.');
  }

  if (Date.now() >= record.expiresAt - 60_000) {
    const refreshed = await refreshDropboxToken(record);
    return refreshed.accessToken;
  }

  return record.accessToken;
}

// ─── Auth flow ────────────────────────────────────────────────────────────────

/**
 * Builds the full Dropbox authorization URL.
 * Generates a fresh PKCE verifier + challenge, stores the verifier in
 * sessionStorage, and encodes a `dbx_`-prefixed state nonce for CSRF validation.
 * Requests `token_access_type=offline` to obtain a long-lived refresh token.
 *
 * The caller should set `window.location.href = url` to trigger the redirect.
 */
export async function buildDropboxAuthUrl(): Promise<string> {
  const verifier  = generateCodeVerifier();
  const challenge = await generateCodeChallenge(verifier);
  const state     = 'dbx_' + crypto.randomUUID().replace(/-/g, '');

  storeVerifier(verifier);
  storeState(state);

  const params = new URLSearchParams({
    client_id:             DROPBOX_APP_KEY,
    redirect_uri:          window.location.origin,
    response_type:         'code',
    code_challenge:        challenge,
    code_challenge_method: 'S256',
    token_access_type:     'offline',   // long-lived refresh token
    state,
  });

  return `${DROPBOX_AUTH_URL}?${params.toString()}`;
}

/**
 * Exchanges the authorization code returned in the callback URL for
 * access + refresh tokens.  Retrieves and clears the PKCE verifier from
 * sessionStorage.  Persists the resulting OAuthTokenRecord to localStorage.
 */
export async function exchangeDropboxCode(code: string): Promise<OAuthTokenRecord> {
  const verifier = retrieveAndClearVerifier();
  if (!verifier) {
    throw new Error('[Dropbox] PKCE verifier missing — possible replay attack.');
  }

  const body = new URLSearchParams({
    client_id:     DROPBOX_APP_KEY,
    code,
    code_verifier: verifier,
    grant_type:    'authorization_code',
    redirect_uri:  window.location.origin,
  });

  const res = await fetch(DROPBOX_TOKEN_URL, {
    method:  'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body:    body.toString(),
  });

  if (!res.ok) {
    throw new Error(`[Dropbox] Token exchange failed: HTTP ${res.status}`);
  }

  const data = await res.json() as TokenResp;
  const record: OAuthTokenRecord = {
    provider:     'dropbox',
    accessToken:  data.access_token,
    refreshToken: data.refresh_token,
    expiresAt:    Date.now() + (data.expires_in ?? DROPBOX_DEFAULT_TTL_MS / 1_000) * 1_000,
    scope:        data.scope ?? 'files.content.write',
  };

  saveDropboxToken(record);
  return record;
}

/**
 * Calls the Dropbox account endpoint to retrieve the authenticated
 * account's email address.
 */
export async function getDropboxUserInfo(token: string): Promise<{ email: string }> {
  const res = await fetch(DROPBOX_USERINFO_URL, {
    method:  'POST',   // Dropbox userinfo is a POST with an empty body
    headers: {
      Authorization:  `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
  });

  if (!res.ok) {
    throw new Error(`[Dropbox] Account info request failed: HTTP ${res.status}`);
  }

  const data = await res.json() as AccountResp;
  return { email: data.email };
}

// ─── File upload ──────────────────────────────────────────────────────────────

/**
 * Writes a single text file to /Apps/AntigravityApp/<filename> in Dropbox.
 * Uses `mode: overwrite` for idempotent re-sync on every call.
 * Returns the byte size of the content.
 */
async function uploadDropboxFile(
  filename: string,
  content:  string,
  token:    string,
): Promise<number> {
  const contentBytes = new TextEncoder().encode(content);

  const apiArg = JSON.stringify({
    path:        `${DROPBOX_FOLDER_PATH}/${filename}`,
    mode:        'overwrite',
    autorename:  false,
    mute:        false,
  });

  const res = await fetch(DROPBOX_UPLOAD_URL, {
    method:  'POST',
    headers: {
      Authorization:     `Bearer ${token}`,
      'Content-Type':    'application/octet-stream',
      'Dropbox-API-Arg': apiArg,
    },
    body: contentBytes,
  });

  if (!res.ok) {
    throw new Error(
      `[Dropbox] Upload failed for "${filename}": HTTP ${res.status}`,
    );
  }

  return contentBytes.byteLength;
}

// ─── Public sync entry point ──────────────────────────────────────────────────

/**
 * Uploads `trails_cache.gpx` and `sync_metadata.json` to the user's Dropbox
 * /Apps/AntigravityApp/ folder.
 *
 * Automatically refreshes the access token if it is about to expire.
 * Returns the number of files uploaded and total bytes transferred.
 */
export async function syncToDropbox(
  gpxContent: string,
  metaJson:   string,
): Promise<{ filesUploaded: number; totalBytes: number }> {
  const token = await getDropboxValidToken();

  const gpxBytes  = await uploadDropboxFile('trails_cache.gpx',     gpxContent, token);
  const metaBytes = await uploadDropboxFile('sync_metadata.json', metaJson,   token);

  return { filesUploaded: 2, totalBytes: gpxBytes + metaBytes };
}
