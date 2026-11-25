// Best effort: the e2e scripts' companions should not outlive the script. A companion its control
// page's quit did not end within the wait gets SIGTERM (its before-quit is bounded), then SIGKILL
// when that did not end it either; one still running when the script exits (a failed run, ^C,
// its wrapper's TERM) gets SIGTERM on the way out. A script that is itself SIGKILLed leaves its
// companion behind. Only the script's own children; never anything else.
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const running = (child) => child && child.exitCode === null && child.signalCode === null;

// `children()`: the processes the script started and still owns (null entries are skipped).
export function reapOnExit(children) {
  process.on('exit', () => {
    for (const child of children()) {
      if (!running(child)) continue;
      try {
        child.kill('SIGTERM');
      } catch {}
    }
  });
  // a signal to the script runs the exit path above (the default action skips it)
  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    process.on(signal, () => {
      console.log(`${signal}: quitting what this run started`);
      process.exit(130);
    });
  }
}

// After a quit through the control page: its exit code, else (after `ms`) SIGTERM and up to 15 s
// more, then SIGKILL and up to 5 s; 'timeout' when even that did not end it.
export async function awaitExit(child, exited, ms = 30000) {
  if (!running(child)) return child ? child.exitCode : 0;
  const code = await Promise.race([exited, sleep(ms).then(() => 'timeout')]);
  if (code !== 'timeout') return code;
  console.log(`companion pid ${child.pid} did not exit after its quit; SIGTERM`);
  try {
    child.kill('SIGTERM');
  } catch {}
  const termed = await Promise.race([exited, sleep(15000).then(() => 'timeout')]);
  if (termed !== 'timeout') return termed;
  console.log(`companion pid ${child.pid} did not exit on SIGTERM; SIGKILL`);
  try {
    child.kill('SIGKILL');
  } catch {}
  return Promise.race([exited, sleep(5000).then(() => 'timeout')]);
}
