// herdr companion: tab groups and the new tab page for agent panes. Driven
// over CDP by herdr's sidecar (Runtime.evaluate on this worker); nothing here
// runs on its own. The code lives in companion.js. Chrome keeps an unpacked
// command-line extension's worker script for good, so herdr clears a
// profile's worker store before launching it with a new companion version.
importScripts('companion.js');
