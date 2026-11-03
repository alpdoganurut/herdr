// Orders one Ghostty window front without activating Ghostty (macOS):
// `osascript -l JavaScript front.jxa.js <pid> <rank> <onscreen>`.
// Ghostty's own scripting, to the instance <pid> only: "select tab" on the selected tab of
// one of its windows (makeKeyAndOrderFront, no app activation: the keyboard stays where it
// is). <rank>: the window's place among the instance's on-screen normal windows, front to
// back (1: its frontmost), from CGWindowList; <onscreen>: how many there are. Ghostty lists
// its windows front to back too (one per tab group), so the window is "front window" for
// rank 1, else "window <rank>" only when Ghostty has exactly <onscreen> windows (none on
// another Space or minimised to shift the count); otherwise nothing is done (exit 4).
// Needs Automation for Ghostty (macOS asks once); refused: exit 5. Prints "ok".
ObjC.import('Foundation');
ObjC.import('CoreServices');

const code = (s) => s.split('').reduce((a, c) => a * 256 + c.charCodeAt(0), 0);

function specifier(want, form, data, container) {
  const spec = $.NSAppleEventDescriptor.recordDescriptor.coerceToDescriptorType(code('obj '));
  spec.setDescriptorForKeyword($.NSAppleEventDescriptor.descriptorWithTypeCode(code(want)), code('want'));
  spec.setDescriptorForKeyword($.NSAppleEventDescriptor.descriptorWithEnumCode(code(form)), code('form'));
  spec.setDescriptorForKeyword(data, code('seld'));
  spec.setDescriptorForKeyword(container, code('from'));
  return spec;
}
const property = (c, container) => specifier('prop', 'prop', $.NSAppleEventDescriptor.descriptorWithTypeCode(code(c)), container);

function send(target, cls, id, direct, extra) {
  const ev = $.NSAppleEventDescriptor.appleEventWithEventClassEventIDTargetDescriptorReturnIDTransactionID(code(cls), code(id), target, -1, 0);
  if (direct) ev.setParamDescriptorForKeyword(direct, code('----'));
  if (extra) for (const [k, v] of extra) ev.setParamDescriptorForKeyword(v, code(k));
  const err = Ref();
  const reply = ev.sendEventWithOptionsTimeoutError($.NSAppleEventSendWaitForReply, 3, err);
  if (reply.isNil()) {
    const e = err[0];
    const n = e && !e.isNil() ? Number(e.code) : -1;
    throw new Error(`apple event ${cls}${id} failed (${n})`, { cause: n });
  }
  return reply;
}

function run(argv) {
  const pid = Number(argv[0]);
  const rank = Number(argv[1]);
  const onscreen = Number(argv[2]);
  if (!(pid > 0) || !(rank > 0)) return 'usage: front.jxa.js <pid> <rank> <onscreen>';
  const target = $.NSAppleEventDescriptor.descriptorWithProcessIdentifier(pid);
  const app = $.NSAppleEventDescriptor.nullDescriptor;
  let win;
  try {
    if (rank === 1) win = property('GFWn', app);
    else {
      // "count windows": Ghostty's windows, every Space, minimised or not
      const reply = send(target, 'core', 'cnte', app, [['kocl', $.NSAppleEventDescriptor.descriptorWithTypeCode(code('Gwnd'))]]);
      const n = Number(reply.paramDescriptorForKeyword(code('----')).int32Value);
      if (n !== onscreen) {
        $.NSFileHandle.fileHandleWithStandardError.writeData($(`ghostty has ${n} windows, ${onscreen} on screen\n`).dataUsingEncoding($.NSUTF8StringEncoding));
        $.exit(4);
      }
      win = specifier('Gwnd', 'indx', $.NSAppleEventDescriptor.descriptorWithInt32(rank), app);
    }
    send(target, 'Ghst', 'SlTb', property('GWsT', win));
  } catch (e) {
    $.NSFileHandle.fileHandleWithStandardError.writeData($(`${e.message}\n`).dataUsingEncoding($.NSUTF8StringEncoding));
    // -1743: not allowed to send Apple events (Automation denied)
    $.exit(e.cause === -1743 ? 5 : 1);
  }
  return 'ok';
}
