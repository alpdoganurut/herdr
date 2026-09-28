# AI News: editorial profile

Reader: a senior engineer who uses Claude Code and Codex every day and follows frontier labs, open weights and AI infra. They want to know what changed, not what to think.

## Page shape (fixed; the runner validates it)
- `since_last_run`: 0 to 6 short lines on what changed since the previous edition, newest first. Any notification text goes first.
- `lead`: the single most consequential story of the last ~24 h, with a `standfirst`.
- `sections`, in this order and with exactly these titles (omit a section only if it has nothing):
  1. **Top**: the next 2 to 4 most consequential items, from any area.
  2. **Models and labs**: frontier and open-weight releases, lab announcements, evals, safety and alignment findings, major product launches.
  3. **Agent tooling**: Claude Code, Codex, Gemini CLI, Copilot, Cursor, opencode, MCP, agent SDKs. Include version numbers. A patch release only counts if it has a user-visible feature or a notable fix.
  4. **Papers**: 3 to 6 papers or research posts; say why each matters.
  5. **Infra and policy**: chips, datacenters, compute deals, big funding (≥ $100M) and M&A, regulation, government action, lawsuits.
  6. **Watching**: rumours, pending launches, stories still developing. Say what would confirm them.

## Item format
- Each item is a JSON object in `page.json`: `head` (≤ 12 words), `text`, `url`, `source`, `time`, `read`.
- `text` is YOUR summary after reading the source: 1 to 2 sentences, 25 to 40 words, what happened plus the one concrete detail that matters (number, version, name, date). Never paste the feed excerpt, never end with an ellipsis.
- The lead also gets a `standfirst`: 2 to 3 sentences, 45 to 70 words, the story and why it matters.
- If you could not read the article, set `read: false` and say only what the headline establishes.
- Link the primary source (lab post, release, paper) when one exists; add one press link only if it adds facts.
- No adjectives like "groundbreaking". No emojis.
- Merge duplicates across sources into one item and keep the best link.

## Length
- ≤ 40 items total, ≤ 10 per section, page.json < 60 KB.
- Revise, don't rewrite: keep still-relevant lines, drop stale ones, re-rank Top.

## Ignore
- Vendor tutorials and how-tos (AWS/GCP/Azure "build X with Y", NVIDIA dev walkthroughs).
- Nightly, alpha and build releases (llama.cpp builds, Codex `-alpha` tags, SDK pre-releases).
- Funding rounds under $100M, stock-price moves, analyst price targets, earnings previews.
- "AI in <industry>" features, listicles, opinion pieces without new facts, prompt-tips posts.
- HN meta posts, Show HN toys under 100 points, Reddit memes.
- Press rewrites of a primary source you already linked.
- Paywalled headlines (The Information, FT, SemiAnalysis) only when nothing else confirms them, marked "(paywalled)".

## Notify (decision.json)
- Notify only for: a new frontier model or major version from Anthropic, OpenAI, Google, Meta, xAI, DeepSeek or Qwen; a Claude Code or Codex release with a headline feature; a major outage or security incident affecting those tools; landmark regulation. At most one per run. `urgency: high` only for a new frontier model or a security issue.
