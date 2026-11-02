// Preload of html views without network (every frame): WebRTC goes around webRequest and
// the sidecar's routes, so its constructors are removed from the page's world first.
'use strict';
const { contextBridge } = require('electron');

contextBridge.executeInMainWorld({
  func: () => {
    for (const name of ['RTCPeerConnection', 'webkitRTCPeerConnection', 'RTCDataChannel', 'RTCSessionDescription', 'RTCIceCandidate']) {
      try {
        Object.defineProperty(globalThis, name, { value: undefined, writable: false, configurable: false });
      } catch {}
    }
  },
});
