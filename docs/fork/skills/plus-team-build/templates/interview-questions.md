# Retro interview — questions per seat

Send each member ONE message with 5–8 questions. Anchor every question to something you already hold (a sha, a NO-GO, a checkpoint), and ask for incidents, not opinions. Max one follow-up round per member. Answers over 4000 chars go to a file.

Collecting replies: `agents_wait_for_message` without `reply_to` can time out while replies sit in the log as `queued`. Poll `agents_messages involving=<you>` and extract the replies to `answers/<role>.md` (JSON contains raw control characters, so parse it with Python, not jq).

Every message ends with:
- "Cite sha/file/time where you can."
- "How did you run your own subagents: what went wrong?"
- "One line: what should the next team's member in your seat be told on day one?"

## Any member
0. Your time, as % of seat time: building / fixing review findings / test runs / waiting (on whom) / rebase+sha triage / comms / late-ruling rework / outage. Then your ONE change with an hours estimate.
1. What blocked you, for how long, and who were you waiting on?
2. A time the brief/contract was unclear or changed: what did it cost?
3. Merge conflicts and rebases: how many, and which files?
4. What did a reviewer/inspector/drill catch that you had missed? What was noise?
5. Messages lost, late, misrouted or duplicated?
6. Outages (usage limit, crashes): what survived and what was lost?
7. What would you change in your brief?

## Integrator (deepest, plus a follow-up round on owner-in-the-loop)
- Speed-ups taken mid-build: what did they buy and what did they cost?
- Which work package or behaviour had no owner, and how did it slip?
- Shape and staleness of rulings after the freeze.
- Which review layer caught what. Which layer you'd drop.
- Were you a bottleneck? Ruling latency vs delivery latency.
- Which rulings were really the owner's?
- How far off was the estimate, and why?

## Reviewers / inspectors
- Findings sent / fixed / deferred / rejected. Your best catch that no other layer had.
- Loop cadence vs trigger. Stale reads. Duplicates with other layers.
- When should your role start?

## Last, briefly: the post-deploy monitor (never during an alarm)
