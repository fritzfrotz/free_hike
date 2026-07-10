/**
 * GoogleDriveSync.ts
 *
 * Full Google Drive OAuth 2.0 PKCE provider bridge.
 *
 * Auth flow:
 *   buildGoogleAuthUrl()      → redirect user to Google consent screen
 *   exchangeGoogleCode(code)  → swap auth code for access + refresh tokens
 *   syncToGoogle(gpx, meta)   → upload two files to /AntigravityApp/ folder
 *   disconnectGoogle()        → wipe localStorage token
 *
 * Token lifecycle:
 *   getGoogleValidToken() checks expiresAt on every upload call.
 *   If the token expires within 60 s it silently refreshes via refresh_token
 *   before returning the access token string.
 *
 * Scope: drive.file
 *   Narrowest scope that allows creating/updating files created by this app.
 *   Cannot read arbitrary Drive content — minimum privilege principle.
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
// Replace with your registered OAuth 2.0 Web Client ID from
// console.cloud.google.com → APIs & Services → Credentials.

const GOOGLE_CLIENT_ID    = 'YOUR_GOOGLE_CLIENT_ID';
const GOOGLE_AUTH_URL     = 'https://accounts.google.com/o/oauth2/v2/auth';
const GOOGLE_TOKEN_URL    = 'https://oauth2.googleapis.com/token';
const GOOGLE_USERINFO_URL = 'https://www.googleapis.com/oauth2/v3/userinfo';
const GOOGLE_FILES_URL    = 'https://www.googleapis.com/drive/v3/files';
const GOOGLE_UPLOAD_URL   = 'https://www.googleapis.com/upload/drive/v3/files';
const GOOGLE_FOLDER_NAME  = 'AntigravityApp';
const GOOGLE_SCOPE        = 'https://www.googleapis.com/auth/drive.file openid email';
const GOOGLE_TOKEN_KEY    = 'antigravity_google_token';

// ─── Internal type helpers ────────────────────────────────────────────────────

interface DriveFile   { id: string }
interface DriveList   { files: DriveFile[] }
interface TokenResp   { access_token: string; refresh_token?: string; expires_in: number; scope: string }
interface RefreshResp { access_token: string; expires_in: number; scope?: string }
interface UserInfoResp{ email: string }

// ─── localStorage helpers ─────────────────────────────────────────────────────

/** Returns the stored token record or null if the user has never connected. */
export function loadGoogleTokenRecord(): OAuthTokenRecord | null {
  const raw = localStorage.getItem(GOOGLE_TOKEN_KEY);
  if (!raw) return null;
  try   { return JSON.parse(raw) as OAuthTokenRecord; }
  catch { return null; }
}

function saveGoogleToken(record: OAuthTokenRecord): void {
  localStorage.setItem(GOOGLE_TOKEN_KEY, JSON.stringify(record));
}

/** Removes the token from localStorage. Call on user-initiated disconnect. */
export function disconnectGoogle(): void {
  localStorage.removeItem(GOOGLE_TOKEN_KEY);
}

// ─── Token lifecycle ──────────────────────────────────────────────────────────

async function refreshGoogleToken(record: OAuthTokenRecord): Promise<OAuthTokenRecord> {
  if (!record.refreshToken) {
    throw new Error('[GoogleDrive] No refresh token stored — user must re-authenticate.');
  }

  const body = new URLSearchParams({
    client_id:     GOOGLE_CLIENT_ID,
    grant_type:    'refresh_token',
    refresh_token: record.refreshToken,
  });

  const res = await fetch(GOOGLE_TOKEN_URL, {
    method:  'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body:    body.toString(),
  });

  if (!res.ok) {
    throw new Error(`[GoogleDrive] Token refresh failed: HTTP ${res.status}`);
  }

  const data = await res.json() as RefreshResp;
  const updated: OAuthTokenRecord = {
    ...record,
    accessToken: data.access_token,
    expiresAt:   Date.now() + data.expires_in * 1_000,
    scope:       data.scope ?? record.scope,
  };

  saveGoogleToken(updated);
  return updated;
}

/**
 * Returns a valid access token, auto-refreshing if within 60 s of expiry.
 * Throws if no token exists — caller must ensure the user is connected first.
 */
async function getGoogleValidToken(): Promise<string> {
  const record = loadGoogleTokenRecord();
  if (!record) {
    throw new Error('[GoogleDrive] No token found. Connect Google Drive first.');
  }

  if (Date.now() >= record.expiresAt - 60_000) {
    const refreshed = await refreshGoogleToken(record);
    return refreshed.accessToken;
  }

  return record.accessToken;
}

// ─── Auth flow ────────────────────────────────────────────────────────────────

/**
 * Builds the full Google authorization URL.
 * Generates a fresh PKCE verifier + challenge, stores the verifier in
 * sessionStorage, and encodes a `g_`-prefixed state nonce for CSRF validation.
 *
 * The caller should set `window.location.href = url` to trigger the redirect.
 */
export async function buildGoogleAuthUrl(): Promise<string> {
  const verifier   = generateCodeVerifier();
  const challenge  = await generateCodeChallenge(verifier);
  const state      = 'g_' + crypto.randomUUID().replace(/-/g, '');

  storeVerifier(verifier);
  storeState(state);

  const params = new URLSearchParams({
    client_id:             GOOGLE_CLIENT_ID,
    redirect_uri:          window.location.origin,
    response_type:         'code',
    scope:                 GOOGLE_SCOPE,
    code_challenge:        challenge,
    code_challenge_method: 'S256',
    state,
    access_type:           'offline',
    prompt:                'consent',   // force refresh_token on every auth
  });

  return `${GOOGLE_AUTH_URL}?${params.toString()}`;
}

/**
 * Exchanges the authorization code returned in the callback URL for
 * access + refresh tokens.  Retrieves and clears the PKCE verifier from
 * sessionStorage.  Persists the resulting OAuthTokenRecord to localStorage.
 */
export async function exchangeGoogleCode(code: string): Promise<OAuthTokenRecord> {
  const verifier = retrieveAndClearVerifier();
  if (!verifier) {
    throw new Error('[GoogleDrive] PKCE verifier missing — possible replay attack.');
  }

  const body = new URLSearchParams({
    client_id:     GOOGLE_CLIENT_ID,
    code,
    code_verifier: verifier,
    grant_type:    'authorization_code',
    redirect_uri:  window.location.origin,
  });

  const res = await fetch(GOOGLE_TOKEN_URL, {
    method:  'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body:    body.toString(),
  });

  if (!res.ok) {
    throw new Error(`[GoogleDrive] Token exchange failed: HTTP ${res.status}`);
  }

  const data = await res.json() as TokenResp;
  const record: OAuthTokenRecord = {
    provider:     'google',
    accessToken:  data.access_token,
    refreshToken: data.refresh_token,
    expiresAt:    Date.now() + data.expires_in * 1_000,
    scope:        data.scope,
  };

  saveGoogleToken(record);
  return record;
}

/**
 * Calls the Google userinfo endpoint to retrieve the authenticated
 * account's email address.
 */
export async function getGoogleUserInfo(token: string): Promise<{ email: string }> {
  const res = await fetch(GOOGLE_USERINFO_URL, {
    headers: { Authorization: `Bearer ${token}` },
  });

  if (!res.ok) {
    throw new Error(`[GoogleDrive] Userinfo request failed: HTTP ${res.status}`);
  }

  const data = await res.json() as UserInfoResp;
  return { email: data.email };
}

// ─── Drive file operations ────────────────────────────────────────────────────

/**
 * Ensures the AntigravityApp folder exists in Drive and returns its file ID.
 * Searches for an existing folder first; creates it only if absent.
 */
async function ensureGoogleFolder(token: string): Promise<string> {
  const q = encodeURIComponent(
    `name='${GOOGLE_FOLDER_NAME}' and mimeType='application/vnd.google-apps.folder' and trashed=false`,
  );

  const searchRes = await fetch(`${GOOGLE_FILES_URL}?q=${q}&fields=files(id)`, {
    headers: { Authorization: `Bearer ${token}` },
  });

  if (!searchRes.ok) {
    throw new Error(`[GoogleDrive] Folder search failed: HTTP ${searchRes.status}`);
  }

  const searchData = await searchRes.json() as DriveList;
  if (searchData.files.length > 0) return searchData.files[0].id;

  // Folder absent — create it.
  const createRes = await fetch(GOOGLE_FILES_URL, {
    method:  'POST',
    headers: {
      Authorization:  `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
    body: JSON.stringify({
      name:     GOOGLE_FOLDER_NAME,
      mimeType: 'application/vnd.google-apps.folder',
    }),
  });

  if (!createRes.ok) {
    throw new Error(`[GoogleDrive] Folder creation failed: HTTP ${createRes.status}`);
  }

  const folder = await createRes.json() as DriveFile;
  return folder.id;
}

/**
 * Uploads (or updates) a single file inside the app folder using the
 * multipart/related upload type.  Returns the byte size of the content.
 */
async function uploadGoogleFile(
  filename: string,
  content:  string,
  mimeType: string,
  folderId: string,
  token:    string,
): Promise<number> {
  // Check whether a file with this name already exists in the folder.
  const q = encodeURIComponent(
    `name='${filename}' and '${folderId}' in parents and trashed=false`,
  );
  const searchRes = await fetch(`${GOOGLE_FILES_URL}?q=${q}&fields=files(id)`, {
    headers: { Authorization: `Bearer ${token}` },
  });

  if (!searchRes.ok) {
    throw new Error(`[GoogleDrive] File search failed: HTTP ${searchRes.status}`);
  }

  const searchData = await searchRes.json() as DriveList;
  const existingId = searchData.files[0]?.id;

  // Build the multipart/related body.
  const boundary = `agv_${Date.now().toString(36)}`;
  const metadata = existingId
    ? JSON.stringify({ name: filename })                         // PATCH: no parents
    : JSON.stringify({ name: filename, parents: [folderId] });   // POST: set parent

  const bodyParts = [
    `--${boundary}`,
    'Content-Type: application/json; charset=UTF-8',
    '',
    metadata,
    `--${boundary}`,
    `Content-Type: ${mimeType}`,
    '',
    content,
    `--${boundary}--`,
  ];

  const url = existingId
    ? `${GOOGLE_UPLOAD_URL}/${existingId}?uploadType=multipart`
    : `${GOOGLE_UPLOAD_URL}?uploadType=multipart`;

  const uploadRes = await fetch(url, {
    method:  existingId ? 'PATCH' : 'POST',
    headers: {
      Authorization:  `Bearer ${token}`,
      'Content-Type': `multipart/related; boundary=${boundary}`,
    },
    body: bodyParts.join('\r\n'),
  });

  if (!uploadRes.ok) {
    throw new Error(
      `[GoogleDrive] Upload failed for "${filename}": HTTP ${uploadRes.status}`,
    );
  }

  return new TextEncoder().encode(content).byteLength;
}

// ─── Public sync entry point ──────────────────────────────────────────────────

/**
 * Uploads `trails_cache.gpx` and `sync_metadata.json` to the user's
 * Google Drive AntigravityApp folder.
 *
 * Automatically refreshes the access token if it is about to expire.
 * Returns the number of files uploaded and total bytes transferred.
 */
export async function syncToGoogle(
  gpxContent: string,
  metaJson:   string,
): Promise<{ filesUploaded: number; totalBytes: number }> {
  const token    = await getGoogleValidToken();
  const folderId = await ensureGoogleFolder(token);

  const gpxBytes  = await uploadGoogleFile(
    'trails_cache.gpx', gpxContent, 'application/gpx+xml', folderId, token,
  );
  const metaBytes = await uploadGoogleFile(
    'sync_metadata.json', metaJson, 'application/json', folderId, token,
  );

  return { filesUploaded: 2, totalBytes: gpxBytes + metaBytes };
}
