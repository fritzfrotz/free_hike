# Experimental modules

Code in this folder is functional but **not wired for release**.

## Cloud sync providers (`DropboxSync.ts`, `GoogleDriveSync.ts`)

Optional user-owned GPX sync via OAuth 2.0 + PKCE. There is no FreeHike
server, so there are no shipped API credentials: to use these you must
register your **own** app and replace the `YOUR_DROPBOX_APP_KEY` /
`YOUR_GOOGLE_CLIENT_ID` constants:

- Dropbox: https://www.dropbox.com/developers/apps (scoped app, `files.content.write`)
- Google: https://console.cloud.google.com → APIs & Services → Credentials
  (OAuth 2.0 Web Client, Drive `drive.file` scope)

Until real credentials are supplied, the Cloud Sync panel's connect flows
will fail at the provider's authorization screen. Everything else in the
app is fully functional without them.
