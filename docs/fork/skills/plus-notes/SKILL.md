---
name: plus:notes
description: Write or refresh this session's herdr+ notes page: a brief, human-readable page with the current context, work in flight and the plan, in the notes pane's format. Use only when the user asks — '/plus:notes', 'update notes', 'write notes', 'make a notes page'. Never on your own; you may offer it.
---

# /plus:notes

The notes page is for the user, not for you: a page they glance at in the pane next to you. It shows **now**, not a history. Rewrite it in place every time.

## Steps

1. Read the current page: `agents_notes_read` (keep the `rev` it prints; `none` while there is no page).
2. Decide what is true **now**. Include a section only if it has real content.
3. Write the whole page with `agents_notes_write`, `base_revision` = the rev from step 1, `reveal: true` (herdr opens the page in the user's info pane without taking their keyboard; an older herdr ignores it). On `conflict` (the user edited it meanwhile), read again and redo once, keeping their edits.
4. Reply with one line: "notes updated".

## The page

```markdown
# <what this session is about, 2–5 words>

<one or two plain sentences: the goal and where it stands>

## In flight
- [ ] <work happening right now>
- [ ] <waiting on: who / what>

## Plan
1. <next step>
2. <step after that>

## Reference
- `<branch / worktree / file / URL that matters>`
```

Sections, all optional, in this order:

| Section | Holds | Leave out when |
|---|---|---|
| title + 1–2 lines | the goal and the state in plain words | never (the minimum page) |
| `## In flight` | open work as `- [ ]`; mark finished items `- [x]` until the next refresh, then drop them | nothing is running |
| `## Waiting on you` | decisions or actions only the user can take | nothing waits on the user |
| `## Plan` | the next steps, numbered, most likely first | the work is done or the next step is obvious from In flight |
| `## Reference` | the 1–5 paths, branches, PRs or links the user would look up | nothing worth looking up |
| `## Notify` | the user's alert preference (set by /plus:notify) | — **always keep it exactly as it is** |

Any other section the user wrote stays as it is, after yours.

## Style

- Brief: the whole page fits on one screen, about 25 lines or fewer. Lines short enough for a 56-column pane.
- Human: plain sentences and short items. No IDs, hashes or timestamps unless the user would act on them. No "Tick", no progress narration, no log.
- No noise: no empty sections, no placeholders, no "N/A", no repeated headings, nothing the user already knows from the tab name.
- Whitespace: one blank line before each `##`; none inside a list.
- Formatting the pane renders: `#`, `##`, `###`, `- [ ]` / `- [x]`, `-` bullets, `1.` lists, `**bold**` for the one thing that matters most, `` `code` `` for paths and commands, `> ` for a short caveat. No tables unless comparing 3+ items; no code blocks unless a command must be copied.
- If the title would only repeat the tab name, still write it (the pane hides a title equal to the tab name).
