// The dock's window probe (macOS): `osascript -l JavaScript probe.jxa.js <pids> <wid> <ms> <fast ms> <every>`.
// One JSON line per sample on stdout, from CGWindowListCopyWindowInfo through the ObjC
// bridge: no Accessibility, no Screen Recording (bounds, owner pid, layer, number and
// on-screen are there without it; names are not, and none is read).
//   target      the window `wid` (with `on`: false when minimised, hidden, on another Space)
//   above       the windows above it, front to back: [number, pid, layer]
//   candidates  every N samples (and while nothing is bound): on-screen layer-0 windows of
//               `pids`, front to back
//   front       the frontmost application's pid
// Samples come every <ms>, every <fast ms> while the target's frame keeps changing. The
// probe ends when its reader goes away (the next write fails).
ObjC.import('CoreGraphics');
ObjC.import('AppKit');
ObjC.import('Foundation');

// Every bridged call costs: read a window's bounds only when they are needed.
function bounds(d) {
  const b = d.objectForKey('kCGWindowBounds');
  return { x: b.objectForKey('X').js, y: b.objectForKey('Y').js, w: b.objectForKey('Width').js, h: b.objectForKey('Height').js };
}

function windowInfo(d) {
  const on = d.objectForKey('kCGWindowIsOnscreen');
  return {
    num: d.objectForKey('kCGWindowNumber').js,
    pid: d.objectForKey('kCGWindowOwnerPID').js,
    layer: d.objectForKey('kCGWindowLayer').js,
    ...bounds(d),
    on: on.isNil() ? false : !!on.js,
  };
}

function list(options, wid) {
  return ObjC.castRefToObject($.CGWindowListCopyWindowInfo(options, wid));
}

function run(argv) {
  const pids = (argv[0] || '').split(',').map(Number).filter((n) => n > 0);
  const wid = Number(argv[1] || 0);
  const interval = Math.max(20, Number(argv[2] || 100)) / 1000;
  const fast = Math.max(20, Number(argv[3] || 50)) / 1000;
  const every = Math.max(1, Number(argv[4] || 5));
  const stdout = $.NSFileHandle.fileHandleWithStandardOutput;
  const emit = (obj) => stdout.writeData($(JSON.stringify(obj) + '\n').dataUsingEncoding($.NSUTF8StringEncoding));
  let tick = 0;
  let last = null;
  for (;;) {
    const sample = { t: Date.now() };
    if (wid) {
      // the window and the ones above it, front to back (it is last)
      const above = list($.kCGWindowListOptionOnScreenAboveWindow | $.kCGWindowListOptionIncludingWindow, wid);
      const n = above.count;
      const last = n ? above.objectAtIndex(n - 1) : null;
      if (last && last.objectForKey('kCGWindowNumber').js === wid) {
        sample.target = windowInfo(last);
        sample.above = [];
        for (let i = 0; i < n - 1; i++) {
          const d = above.objectAtIndex(i);
          const layer = d.objectForKey('kCGWindowLayer').js;
          // only normal windows can sit between the terminal and the dock
          if (layer === 0) sample.above.push([d.objectForKey('kCGWindowNumber').js, d.objectForKey('kCGWindowOwnerPID').js, 0]);
        }
      } else {
        const one = list($.kCGWindowListOptionIncludingWindow, wid);
        sample.target = one.count ? { ...windowInfo(one.objectAtIndex(0)), on: false } : null;
        sample.above = [];
      }
    }
    if (pids.length && (!wid || tick % every === 0)) {
      const all = list($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, 0);
      sample.candidates = [];
      for (let i = 0; i < all.count; i++) {
        const d = all.objectAtIndex(i);
        const pid = d.objectForKey('kCGWindowOwnerPID').js;
        if (!pids.includes(pid) || d.objectForKey('kCGWindowLayer').js !== 0) continue;
        sample.candidates.push({ num: d.objectForKey('kCGWindowNumber').js, pid, ...bounds(d) });
      }
    }
    const front = $.NSWorkspace.sharedWorkspace.frontmostApplication;
    sample.front = front.isNil() ? 0 : front.processIdentifier;
    emit(sample);
    const t = sample.target;
    const moving = !!(t && last && (t.x !== last.x || t.y !== last.y || t.w !== last.w || t.h !== last.h));
    last = t;
    tick += 1;
    delay(moving ? fast : interval);
  }
}
