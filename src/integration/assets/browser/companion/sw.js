// herdr companion: tab groups for agent panes. Driven over CDP by herdr's
// sidecar (Runtime.evaluate on this worker); nothing here runs on its own.
//
// This bootstrap never changes: Chrome keeps an extension worker's script
// until the registration changes, which an unpacked command-line extension's
// never does, so the code lives in companion.js and is imported under the
// manifest version — a new version is a new URL, fetched fresh from disk.
importScripts('companion.js?v=' + chrome.runtime.getManifest().version);
