---
name: plus:notify
description: "Set, show or clear this session's preference for agent cards (agents_notify): when, and about what, this agent should alert the user, in the user's own words. Use when the user says /plus:notify, 'ping me when…', 'only notify me if…', 'don't notify me about…', 'stop notifying me'. Not for herdr's automatic toasts and sounds (config.toml [ui.toast]/[ui.sound])."
argument-hint: "[<preference in plain words> | off | (empty = show)]"
user-invocable: true
---

# plus:notify: this session's alert preference

The preference lives as a `## Notify` section in this agent's herdr+ notes (`agents_notes_read` /
`agents_notes_write`). herdr gives notes back after `/clear` and compaction, so it lasts the whole session.
Without the section, the herdr+ default applies: notify only when the user should look now (question, done,
warning), never for routine progress.

## Do
1. `agents_notes_read` (your own notes). Keep its `rev`.
2. By argument:
   - **Empty:** show the current `## Notify` text, or "none: herdr default". Stop.
   - **`off` / `clear` / `default`:** remove the `## Notify` section.
   - **Anything else:** replace the `## Notify` section (or add it right after the notes' title line) with:
     ```
     ## Notify
     <the user's preference, their wording, lightly cleaned; one or a few lines>
     (set <YYYY-MM-DD HH:MM>)
     ```
     Don't turn it into a schema, categories or numbers the user didn't say.
3. `agents_notes_write` with the whole notes and `base_revision` = that rev. On `conflict`, read again and redo.
   Touch nothing else in the notes.
4. Reply in one line: what you'll now notify about, in plain words.

## Then
For the rest of the session, decide every `agents_notify` by the `## Notify` text: it overrides the default,
in both directions (more or fewer cards). If it's ambiguous for a case, follow the default.
This changes only cards this agent sends. herdr's automatic toasts and sounds are global
(`~/.config/herdr/config.toml`).
