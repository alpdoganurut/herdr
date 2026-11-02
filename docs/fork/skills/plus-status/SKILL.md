---
name: plus:status
description: "One-shot status digest of herdr+ agents: who is working, blocked or done, on what, and what waits on the user. Use when the user asks 'sup', 'how is it going', 'what's everyone doing', 'status of the team/group', 'what's next', or 'anything waiting on me' inside herdr. Read-only. Args: [group | all]."
argument-hint: "[<group> | all]"
user-invocable: true
---

# plus:status: fleet digest

Answer the user's "how is it going" in one table instead of making them visit tabs. **Read-only**: never
message, wake, suspend or reorder anything while building the digest.

## 1. Pick the scope
- **No argument:** your own group (`agents_list` with no args). If you are the coordinator, use `all`.
- **A group name or number:** `agents_list group=<arg>`.
- **`all`:** do NOT use `agents_list all=true`. It ignores `status=` and truncates after about 40 tabs, buried
  under suspended ones. Instead run this once (one line per active agent: status, name, pane, context %, title):
  ```bash
  herdr agent list | jq -r '.result.agents[] | select(.agent_status|IN("working","blocked","done")) | [.agent_status, (.name // "-"), .pane_id, ((.context.percent|tostring)+"%"), .terminal_title] | @tsv'
  ```
  The pane id prefix (`wS:`, `w11:` …) is the group's `workspace_id`. Get the labels from the `groups` list of
  a plain `agents_list` call. Add idle tabs only for groups the user named.

## 2. Gather, cheaply
For each agent in scope that is **working, blocked or done** (skip suspended, and skip idle unless it is the
lead or the user asked for that group). The terminal title is often enough to say what it's on. A context
above 80% is worth flagging (it will compact soon).
- `agents_messages all=true involving=<name or pane> limit=5`: its latest reports and asks (what it finished, what it waits on).
- `agents_get target=<…>` only when the role, note and title don't say what it's doing (it costs 5 messages).
- `agents_notes_read target=<…>` only for leads, and only when it has a notes page (the user's page: read its
  "In flight" and "Waiting on you" parts). No page is normal; don't flag it.

Stop at about 15 agents. If more are active, say how many you skipped and why.

## 3. Report
One table, active first:

| Agent (group) | State | On | Latest | Needs you? |
|---|---|---|---|---|

- **State:** working / blocked / done / idle, plus how long when known.
- **On:** the role and note (or the terminal title), cut to one line.
- **Latest:** its last report or ask from the message log (or the page's In flight line), one line.
- **Needs you?:** "yes: <what>" when it is blocked on the user, has a `done` turn you haven't seen, asked the user
  something in its last message, or its notes page lists an item waiting on the user. Otherwise empty.

Then at most 3 lines:
- **Waiting on you:** the yes-rows, most urgent first (blocked > done > open item).
- **Stuck:** anything working much longer than its peers with no new message or report, or whose last report
  says it failed or is blocked.
- **Quiet:** counts only, e.g. "12 suspended, 4 idle in backend".

No prose recap of each agent. Never paste message bodies. Quote at most one line per agent.
