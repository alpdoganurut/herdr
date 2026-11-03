// The dock's window probe (macOS): `osascript -l JavaScript probe.jxa.js <pids> <wid> <ms> <fast ms> <every>`.
// One JSON line per sample on stdout, from CGWindowListCopyWindowInfo through the ObjC
// bridge: no Accessibility, no Screen Recording (bounds, owner pid, layer, number and
// on-screen are there without it; names are not, and none is read).
//   wid         the window asked about (0: none)
//   target      the window `wid` (with `on`: false when minimised, hidden, on another Space,
//               or a Ghostty tab that is not the selected one); null: the window is gone
//   above       the windows above it, front to back: [number, pid, layer]
//   candidates  every N samples (and while nothing is bound): on-screen layer-0 windows of
//               `pids`, front to back
// Samples come every <ms>, every <fast ms> while the target's frame keeps changing. The
// probe ends when its reader goes away (the next write fails).
ObjC.import('CoreGraphics');
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

// The bridge gives an array's count as a string ("0" is truthy): always a number.
const count = (a) => Number(a.count) || 0;

// The window `wid` among all windows, on screen or not: `IncludingWindow` alone lists no
// off-screen window of a tab group (a Ghostty tab that is not the selected one), so the full
// list tells "off screen" from "gone". Only while the window is off screen (the dock is hidden),
// and then only every <every> samples (a walk of every window through the bridge): the last
// answer serves in between (a stale frame or a late "gone" is harmless while hidden).
function findWindow(wid) {
  const all = list($.kCGWindowListOptionAll | $.kCGWindowListExcludeDesktopElements, 0);
  for (let i = 0, n = count(all); i < n; i++) {
    const d = all.objectAtIndex(i);
    if (d.objectForKey('kCGWindowNumber').js === wid) return d;
  }
  return null;
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
  let offTick = 0; // samples answered by findWindow's cadence since the window was last on screen
  let found = null; // findWindow's last answer (a target record, or null: gone)
  for (;;) {
    const sample = { t: Date.now(), wid };
    if (wid) {
      // the window and the ones above it, front to back (it is last)
      const above = list($.kCGWindowListOptionOnScreenAboveWindow | $.kCGWindowListOptionIncludingWindow, wid);
      const n = count(above);
      const last = n ? above.objectAtIndex(n - 1) : null;
      if (last && last.objectForKey('kCGWindowNumber').js === wid) {
        offTick = 0;
        found = null;
        sample.target = windowInfo(last);
        sample.above = [];
        for (let i = 0; i < n - 1; i++) {
          const d = above.objectAtIndex(i);
          const layer = d.objectForKey('kCGWindowLayer').js;
          // only normal windows can sit between the terminal and the dock
          if (layer === 0) sample.above.push([d.objectForKey('kCGWindowNumber').js, d.objectForKey('kCGWindowOwnerPID').js, 0]);
        }
      } else {
        // off screen (the above-list of an off-screen window is every on-screen window)
        const one = list($.kCGWindowListOptionIncludingWindow, wid);
        if (count(one)) sample.target = { ...windowInfo(one.objectAtIndex(0)), on: false };
        else {
          if (offTick % every === 0) {
            const d = findWindow(wid);
            found = d ? { ...windowInfo(d), on: false } : null;
          }
          offTick += 1;
          sample.target = found;
        }
        sample.above = [];
      }
    }
    if (pids.length && (!wid || tick % every === 0)) {
      const all = list($.kCGWindowListOptionOnScreenOnly | $.kCGWindowListExcludeDesktopElements, 0);
      sample.candidates = [];
      for (let i = 0, n = count(all); i < n; i++) {
        const d = all.objectAtIndex(i);
        const pid = d.objectForKey('kCGWindowOwnerPID').js;
        if (!pids.includes(pid) || d.objectForKey('kCGWindowLayer').js !== 0) continue;
        sample.candidates.push({ num: d.objectForKey('kCGWindowNumber').js, pid, ...bounds(d) });
      }
    }
    emit(sample);
    const t = sample.target;
    const moving = !!(t && last && (t.x !== last.x || t.y !== last.y || t.w !== last.w || t.h !== last.h));
    last = t;
    tick += 1;
    delay(moving ? fast : interval);
  }
}
