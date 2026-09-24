#!/usr/bin/env bash
# Deterministic half of /fork-sync: merge upstream master into the `fork`
# branch without ever leaving `fork` partially merged. FORK.md (read from the
# fork tip, never the working copy) is the rulebook; the agent only resolves
# what FORK.md allows between `prepare` and `gate`.
#
# Usage: scripts/fork_sync.sh <status|prepare|gate|land|install|abort|note TEXT|gate-steps>
#
# Exit codes
#   0   success (prepare: clean merge ready for gate, or nothing new)
#   10  prepare: conflicts the agent may resolve per FORK.md
#       gate:    only E0063/E0027 on FORK.md section 2 fields; insert defaults, rerun gate
#   20  denied (prepare: classification; gate: any other failure)
#   30  land: refused, merge parked at refs/fork-sync/<run>; one command printed
#   31  land: landed locally but the push to mine failed
#   40  install: protocol/generation changed (or no stamp); staged as herdr.next
#   75  another fork-sync invocation holds the lock
#   1   usage or precondition error
#
# The user's checkout is touched by exactly one command: `git merge --ff-only`
# in `land`. Environment knobs (defaults are production):
#   FORK_SYNC_DRY_RUN=1        everything except land/push/install
#   FORK_SYNC_SKIP_GATE=1      gate records "skipped" without running steps
#   FORK_SYNC_GATE_CMD=...     replace the gate step list with one command
#   FORK_SYNC_MAIN, FORK_SYNC_WORKTREE, FORK_SYNC_TARGET_DIR, FORK_SYNC_STATE_DIR,
#   FORK_SYNC_BIN_DIR, FORK_SYNC_BUILT_BINARY, FORK_SYNC_CODESIGN,
#   FORK_SYNC_UPSTREAM_REMOTE, FORK_SYNC_UPSTREAM_BRANCH, FORK_SYNC_MINE_REMOTE,
#   FORK_SYNC_FORK_BRANCH
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
LIB="$SCRIPT_DIR/fork_sync_lib.py"

# A /fork-sync started from a herdr tab must never reach the live server.
unset HERDR_SOCKET_PATH HERDR_CLIENT_SOCKET_PATH HERDR_PANE_ID HERDR_ENV \
  HERDR_SESSION HERDR_WORKSPACE_ID HERDR_TAB_ID
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
# Remote git (fetch/ls-remote/push to mine over SSH) fails fast instead of prompting.
export GIT_TERMINAL_PROMPT=0
export GIT_SSH_COMMAND="${GIT_SSH_COMMAND:-ssh -oBatchMode=yes}"

die() {
  local code=$1
  shift
  printf 'fork-sync: %s\n' "$*" >&2
  exit "$code"
}

say() {
  printf 'fork-sync: %s\n' "$*"
}

py() {
  python3 "$LIB" "$@"
}

if [[ -n "${FORK_SYNC_MAIN:-}" ]]; then
  MAIN=$(cd "$FORK_SYNC_MAIN" && pwd)
else
  common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) ||
    die 1 "run inside the herdr checkout (or set FORK_SYNC_MAIN)"
  MAIN=$(dirname "$common")
fi
GIT_COMMON=$(git -C "$MAIN" rev-parse --path-format=absolute --git-common-dir)

UPSTREAM_REMOTE=${FORK_SYNC_UPSTREAM_REMOTE:-origin}
UPSTREAM_BRANCH=${FORK_SYNC_UPSTREAM_BRANCH:-master}
MINE_REMOTE=${FORK_SYNC_MINE_REMOTE:-mine}
FORK_BRANCH=${FORK_SYNC_FORK_BRANCH:-fork}
WT=${FORK_SYNC_WORKTREE:-$(dirname "$MAIN")/herdr-worktrees/fork-sync}
TARGET=${FORK_SYNC_TARGET_DIR:-$WT-target}
LOCAL_DIR="$MAIN/.local/fork-sync"
STATE="$LOCAL_DIR/state.json"
FORK_MD="$LOCAL_DIR/FORK.md"
STAMP_DIR=${FORK_SYNC_STATE_DIR:-$HOME/.local/state/herdr-fork-sync}
STAMP="$STAMP_DIR/installed-sha"
BIN_DIR=${FORK_SYNC_BIN_DIR:-$HOME/.local/bin}
BUILT_BINARY=${FORK_SYNC_BUILT_BINARY:-$TARGET/release/herdr}
CODESIGN=${FORK_SYNC_CODESIGN:-codesign -s - -f}
DRY_RUN=${FORK_SYNC_DRY_RUN:-0}
LOCK_FILE="$GIT_COMMON/fork-sync.lock"
SELF="$MAIN/scripts/fork_sync.sh"

# state.json writes re-render the report (fork_sync_lib.save_state).
st_set() { py set "$STATE" "$@"; }
st_append() { py append "$STATE" "$@"; }
# Load state keys as S_<key> shell variables (dots become underscores).
S_run_id="" S_phase="" S_report="" S_upstream_sha="" S_fork_sha="" S_commit_count=""
S_land_merge_sha="" S_land_landed="" S_land_pushed="" S_install_result="" S_gate_gated_tree=""
load_state() {
  local assignments
  assignments=$(py vars "$STATE" run_id phase report upstream_sha fork_sha commit_count \
    land.merge_sha land.landed land.pushed install.result gate.gated_tree)
  eval "$assignments"
}
is_dry_run() { [[ "$DRY_RUN" == 1 ]]; }

# --------------------------------------------------------------------------
# lock: one fork-sync invocation at a time (lockf on macOS, flock elsewhere).
# The lock is per invocation; the in-progress sync lives in state.json.

acquire_lock() {
  exec 9>"$LOCK_FILE"
  if command -v lockf >/dev/null 2>&1; then
    lockf -s -t 0 9 2>/dev/null || die 75 "another fork-sync run holds $LOCK_FILE"
  elif command -v flock >/dev/null 2>&1; then
    flock -n 9 || die 75 "another fork-sync run holds $LOCK_FILE"
  else
    die 1 "neither lockf nor flock is available"
  fi
}

lock_is_free() {
  (
    exec 8>"$LOCK_FILE"
    if command -v lockf >/dev/null 2>&1; then
      lockf -s -t 0 8
    else
      flock -n 8
    fi
  ) 2>/dev/null
}

# --------------------------------------------------------------------------
# sync worktree (detached, persistent, reset every prepare)

is_registered_worktree() {
  [[ -d "$WT" ]] || return 1
  local logical physical
  logical=$(cd "$WT" && pwd)
  physical=$(cd "$WT" && pwd -P)
  git -C "$MAIN" worktree list --porcelain |
    grep -Fx -e "worktree $logical" -e "worktree $physical" >/dev/null
}

reset_worktree() {
  local f0=$1
  git -C "$WT" merge --abort >/dev/null 2>&1 || true
  git -C "$WT" reset --hard --quiet
  git -C "$WT" checkout --quiet --force --detach "$f0"
  git -C "$WT" clean -fdq
}

ensure_worktree() {
  local f0=$1
  git -C "$MAIN" worktree prune
  if is_registered_worktree; then
    reset_worktree "$f0"
    return
  fi
  if [[ -e "$WT" ]]; then
    die 1 "$WT exists but is not a worktree of $MAIN; move it away"
  fi
  mkdir -p "$(dirname "$WT")"
  git -C "$MAIN" worktree add --quiet --detach "$WT" "$f0"
}

wt_rev() {
  git -C "$WT" rev-parse -q --verify "$1" 2>/dev/null || true
}

# --------------------------------------------------------------------------
# status

cmd_status() {
  local fork mine upstream stamp
  load_state
  fork=$(git -C "$MAIN" rev-parse -q --verify "refs/heads/$FORK_BRANCH" || true)
  mine=$(git -C "$MAIN" rev-parse -q --verify "refs/remotes/$MINE_REMOTE/$FORK_BRANCH" || true)
  upstream=$(git -C "$MAIN" rev-parse -q --verify "refs/remotes/$UPSTREAM_REMOTE/$UPSTREAM_BRANCH" || true)
  stamp=$(cat "$STAMP" 2>/dev/null || true)
  printf 'fork             %s\n' "${fork:-?}"
  printf '%-16s %s\n' "$MINE_REMOTE/$FORK_BRANCH" "${mine:-?}"
  printf '%-16s %s (as of the last fetch)\n' "$UPSTREAM_REMOTE/$UPSTREAM_BRANCH" "${upstream:-?}"
  if [[ -n "$fork" && -n "$upstream" ]]; then
    printf 'incoming         %s commit(s)\n' "$(git -C "$MAIN" rev-list --count "$fork..$upstream")"
  fi
  if [[ -n "$fork" && -n "$mine" ]]; then
    printf 'push pending     %s commit(s) on fork not on %s\n' \
      "$(git -C "$MAIN" rev-list --count "$mine..$fork")" "$MINE_REMOTE/$FORK_BRANCH"
  fi
  printf 'installed-sha    %s\n' "${stamp:-none ($STAMP)}"
  if [[ -n "$stamp" && -n "$fork" && "$stamp" != "$fork" ]]; then
    printf 'install pending  the installed build is not the fork tip\n'
  fi
  if [[ -e "$BIN_DIR/herdr.next" ]]; then
    printf 'staged           %s (install + server restart together)\n' "$BIN_DIR/herdr.next"
  fi
  if lock_is_free; then
    printf 'lock             free\n'
  else
    printf 'lock             held (%s)\n' "$LOCK_FILE"
  fi
  printf 'run              %s phase=%s\n' "${S_run_id:-none}" "${S_phase:-none}"
  printf 'report           %s\n' "${S_report:-none}"
  printf 'state            %s\n' "$STATE"
}

# --------------------------------------------------------------------------
# prepare

fetch_refs() {
  git -C "$MAIN" fetch --no-tags --quiet "$UPSTREAM_REMOTE" \
    "+refs/heads/$UPSTREAM_BRANCH:refs/remotes/$UPSTREAM_REMOTE/$UPSTREAM_BRANCH"
  if ! git -C "$MAIN" fetch --no-tags --quiet "$MINE_REMOTE" \
    "+refs/heads/$FORK_BRANCH:refs/remotes/$MINE_REMOTE/$FORK_BRANCH" 2>/dev/null; then
    say "warning: could not fetch $MINE_REMOTE/$FORK_BRANCH; push-pending check uses the stale ref"
  fi
}

cmd_prepare() {
  acquire_lock
  mkdir -p "$LOCAL_DIR"
  load_state
  case "$S_phase" in
    merging | conflicts | merged | gating | gate_failed | gated | committed | parked)
      say "discarding in-progress run $S_run_id (phase $S_phase); the sync worktree is reset"
      ;;
  esac
  if [[ -n "$S_land_merge_sha" && "$S_land_landed" != true ]] &&
    ! git -C "$MAIN" merge-base --is-ancestor "$S_land_merge_sha" "refs/heads/$FORK_BRANCH" 2>/dev/null; then
    git -C "$MAIN" update-ref "refs/fork-sync/$S_run_id" "$S_land_merge_sha"
    say "kept the unlanded merge of run $S_run_id at refs/fork-sync/$S_run_id"
  fi

  fetch_refs
  local U F0 base count run_id ushort report
  U=$(git -C "$MAIN" rev-parse "refs/remotes/$UPSTREAM_REMOTE/$UPSTREAM_BRANCH")
  F0=$(git -C "$MAIN" rev-parse "refs/heads/$FORK_BRANCH")
  base=$(git -C "$MAIN" merge-base "$F0" "$U") || die 1 "no merge base between $FORK_BRANCH and $U"
  count=$(git -C "$MAIN" rev-list --count "$F0..$U")
  ushort=$(git -C "$MAIN" rev-parse --short "$U")

  if [[ "$count" == 0 && "$S_upstream_sha" == "$U" && -n "$S_land_merge_sha" ]]; then
    # Nothing new, but the last run may have stopped before push or install.
    say "no new upstream commits; run $S_run_id merged $ushort"
    [[ "$S_land_landed" == true ]] || say "pending: landing (run: $SELF land)"
    [[ "$S_land_pushed" == true ]] || say "pending: push (run: $SELF land)"
    case "$S_install_result" in
      installed | staged | already) ;;
      *) say "pending: install (run: $SELF install)" ;;
    esac
    say "report: $S_report"
    return 0
  fi

  run_id=$(date +%Y-%m-%dT%H%M%S)
  report="$LOCAL_DIR/$run_id-$ushort.md"
  rm -f "$STATE"
  st_set schema:=1 "run_id=$run_id" phase=classifying "main=$MAIN" "worktree=$WT" \
    "target_dir=$TARGET" "upstream_ref=$UPSTREAM_REMOTE/$UPSTREAM_BRANCH" \
    "upstream_sha=$U" "fork_sha=$F0" "base_sha=$base" "commit_count:=$count" \
    "report=$report" "forkmd=$FORK_MD"

  if [[ "$count" == 0 ]]; then
    local ahead
    ahead=$(git -C "$MAIN" rev-list --count "refs/remotes/$MINE_REMOTE/$FORK_BRANCH..$F0" 2>/dev/null || echo 0)
    st_set phase=up_to_date outcome="up to date" "fork_ahead_of_mine:=$ahead"
    say "up to date: $FORK_BRANCH already contains $UPSTREAM_REMOTE/$UPSTREAM_BRANCH $ushort"
    [[ "$ahead" == 0 ]] || say "note: $FORK_BRANCH is $ahead commit(s) ahead of $MINE_REMOTE/$FORK_BRANCH (fork-sync does not push those on its own)"
    say "report: $report"
    return 0
  fi

  if ! git -C "$MAIN" show "$F0:FORK.md" >"$FORK_MD" 2>/dev/null; then
    st_set phase=denied outcome=denied \
      'classify:={"denials":[{"kind":"no manifest","path":"FORK.md","detail":"FORK.md missing at the fork tip"}]}'
    say "denied: FORK.md is missing at $FORK_BRANCH $F0"
    say "report: $report"
    return 20
  fi

  local classified verdict predicted
  classified=$(py classify --repo "$MAIN" --fork "$F0" --upstream "$U" --base "$base" \
    --forkmd "$FORK_MD" --state "$STATE")
  verdict=$(printf '%s\n' "$classified" | head -n 1)
  predicted=$(printf '%s\n' "$classified" | tail -n +2 | sort)

  ensure_worktree "$F0"

  if [[ "$verdict" == 20 ]]; then
    st_set phase=denied outcome=denied
    say "denied: $count upstream commit(s) at $ushort; see the report"
    say "report: $report"
    return 20
  fi

  st_set phase=merging
  local merge_rc=0
  git -C "$WT" -c merge.conflictStyle=zdiff3 -c rerere.enabled=false \
    -c commit.gpgsign=false merge --no-commit --no-ff --quiet "$U" >/dev/null 2>&1 || merge_rc=$?
  [[ "$(wt_rev MERGE_HEAD)" == "$U" ]] || die 1 "merge did not start in $WT (rc $merge_rc)"

  local actual line
  actual=$(git -C "$WT" diff --name-only --diff-filter=U | sort)
  if [[ "$actual" != "$predicted" ]]; then
    reset_worktree "$F0"
    st_append warnings "merge-tree predicted [$predicted] but the worktree merge conflicted on [$actual]"
    st_set phase=denied outcome=denied
    say "denied: merge-tree and the worktree merge disagree; see the report"
    return 20
  fi

  if [[ "$verdict" == 10 ]]; then
    st_set phase=conflicts outcome="conflicts to resolve"
    say "conflicts in $count upstream commit(s) at $ushort; resolve per FORK.md in $WT, then run gate:"
    while IFS= read -r line; do printf '  %s\n' "$line"; done <<<"$actual"
    say "report: $report"
    return 10
  fi
  st_set phase=merged outcome="merged, gate pending"
  say "clean merge of $count upstream commit(s) at $ushort in $WT; run gate"
  say "report: $report"
  return 0
}

# --------------------------------------------------------------------------
# gate

# The one place the gate command list lives. Output: name<TAB>command.
# `SKIP` as a command records the step as skipped.
gate_steps() {
  if [[ -n "${FORK_SYNC_GATE_CMD:-}" ]]; then
    printf 'stub\t%s\n' "$FORK_SYNC_GATE_CMD"
    return
  fi
  local schema_test=generated_protocol_schema_artifact_is_current
  printf 'fmt\t%s\n' "cargo fmt --check"
  printf 'clippy\t%s\n' "cargo clippy --all-targets --locked -- -D warnings"
  printf 'schema-regen\t%s\n' "HERDR_UPDATE_API_SCHEMA=1 cargo nextest run --locked --no-tests=fail $schema_test && git add docs/next/api"
  printf 'schema-verify\t%s\n' "cargo nextest run --locked --no-tests=fail $schema_test && git diff --exit-code docs/next/api/"
  printf 'nextest\t%s\n' "cargo nextest run --locked --status-level fail --final-status-level slow --failure-output final --success-output never"
  printf 'maintenance-test\t%s\n' "python3 -m unittest scripts.test_agent_detection_manifest_check scripts.test_changelog scripts.test_config_reference_check scripts.test_docs_translation_parity scripts.test_hermes_integration_asset scripts.test_package_windows_conpty scripts.test_preview scripts.test_release scripts.test_unix_installer scripts.test_vendor_libghostty_vt scripts.test_vendor_portable_pty scripts.test_windows_cross scripts.test_windows_input"
  printf 'ui-hot-path-architecture-test\t%s\n' "python3 -m unittest scripts.test_ui_hot_path_architecture"
  if command -v bun >/dev/null 2>&1; then
    printf 'integration-assets-test\t%s\n' "bun test src/integration/assets/herdr-agent-state.test.ts && bun test src/integration/assets/opencode/herdr-agent-state.test.ts && bun test src/integration/assets/opencode/herdr-tui-session.test.ts"
  else
    printf 'integration-assets-test\tSKIP\n'
  fi
  printf 'config-reference-check\t%s\n' "python3 scripts/config_reference_check.py"
  printf 'docs-translation-parity\t%s\n' "python3 scripts/docs_translation_parity.py --docs-root docs/next/website/src/content/docs"
  # FORK.md section 10: full nextest names; each must match exactly one test.
  local name
  while IFS= read -r name; do
    [[ -n "$name" ]] || continue
    printf 'smoke:%s\t%s\n' "$name" "cargo nextest run --locked --no-tests=fail -E 'test(=$name)'"
  done < <(py forkmd "$FORK_MD" --key smoke_tests)
  printf 'release-build\t%s\n' "cargo build --release --locked"
}

check_merge_worktree() {
  local U=$1 F0=$2
  is_registered_worktree || die 1 "sync worktree $WT is missing; run prepare"
  [[ "$(wt_rev HEAD)" == "$F0" ]] || die 1 "sync worktree HEAD is not F0 $F0; run prepare"
  [[ "$(wt_rev MERGE_HEAD)" == "$U" ]] || die 1 "no in-progress merge of $U in $WT; run prepare"
}


cmd_gate() {
  acquire_lock
  load_state
  case "$S_phase" in
    conflicts | merged | gating | gate_failed | gated) ;;
    *) die 1 "nothing to gate (phase ${S_phase:-none}); run prepare" ;;
  esac
  check_merge_worktree "$S_upstream_sha" "$S_fork_sha"

  # A conflicted path counts as resolved once its markers are gone; stage it.
  local unmerged markers
  unmerged=$(git -C "$WT" diff --name-only --diff-filter=U -z |
    (cd "$WT" && xargs -0 grep -lE '^(<<<<<<<|>>>>>>>) ' 2>/dev/null) || true)
  [[ -z "$unmerged" ]] || die 1 "unresolved conflicts remain: $(echo "$unmerged" | tr '\n' ' ')"
  git -C "$WT" add -A
  markers=$(git -C "$WT" diff --cached --name-only -z "$S_fork_sha" |
    (cd "$WT" && xargs -0 grep -lE '^(<<<<<<<|>>>>>>>) ' 2>/dev/null) || true)
  [[ -z "$markers" ]] || die 1 "conflict markers left in: $(echo "$markers" | tr '\n' ' ')"
  local pre_tree
  pre_tree=$(git -C "$WT" write-tree)

  local gate_dir="$LOCAL_DIR/$S_run_id-gate"
  rm -rf "$gate_dir"
  mkdir -p "$gate_dir"
  if [[ "${FORK_SYNC_SKIP_GATE:-0}" == 1 ]]; then
    st_set 'gate:={}' gate.result=skipped "gate.gated_tree=$(git -C "$WT" write-tree)" \
      phase=gated outcome="merged, gate skipped"
    say "gate skipped (FORK_SYNC_SKIP_GATE=1)"
    return 0
  fi
  st_set 'gate:={}' phase=gating

  local steps_json="[" sep="" i=0 name cmd rc log="" failed=""
  while IFS=$'\t' read -r name cmd <&3; do
    i=$((i + 1))
    log="$gate_dir/$(printf '%02d' "$i")-${name//[^A-Za-z0-9_.-]/_}.log"
    if [[ "$cmd" == SKIP ]]; then
      echo "skipped: required tool not installed" >"$log"
      steps_json+="$sep{\"name\":\"$name\",\"rc\":\"skipped\",\"log\":\"$log\"}"
      sep=","
      say "gate: $name skipped (tool not installed)"
      continue
    fi
    say "gate: $name"
    rc=0
    (cd "$WT" && CARGO_TARGET_DIR="$TARGET" bash -c "$cmd") >"$log" 2>&1 </dev/null 9>&- || rc=$?
    steps_json+="$sep{\"name\":\"$name\",\"rc\":$rc,\"log\":\"$log\"}"
    sep=","
    if [[ "$rc" != 0 ]]; then
      failed=$name
      break
    fi
  done 3< <(gate_steps)
  steps_json+="]"

  local post_tree stray
  git -C "$WT" add -A
  post_tree=$(git -C "$WT" write-tree)
  # The gate may only regenerate docs/next/api. Anything else it wrote is
  # reverted to the pre-gate tree (so a rerun cannot bake it into M) and fails it.
  stray=$(git -C "$WT" diff --name-only "$pre_tree" "$post_tree" | grep -v '^docs/next/api/' || true)
  if [[ -n "$stray" ]]; then
    git -C "$WT" diff --name-only "$pre_tree" "$post_tree" >"$gate_dir/unexpected-files.log"
    git -C "$WT" read-tree "$pre_tree"
    git -C "$WT" checkout-index -f -a
    git -C "$WT" clean -fdq
    post_tree=$pre_tree
    if [[ -z "$failed" ]]; then
      log="$gate_dir/unexpected-files.log"
      failed="unexpected files written by the gate: $(echo "$stray" | tr '\n' ' ')"
    else
      st_append warnings "gate wrote unexpected files (reverted): $(echo "$stray" | tr '\n' ' ')"
    fi
  fi

  if [[ -z "$failed" ]]; then
    st_set "gate.steps:=$steps_json" gate.result=passed "gate.gated_tree=$post_tree" \
      phase=gated outcome="merged, gate passed"
    say "gate passed"
    say "report: $S_report"
    return 0
  fi

  local e0063
  e0063=$(py gate-analyze "$log" "$FORK_MD" --state "$STATE")
  if [[ "$e0063" == true ]]; then
    st_set "gate.steps:=$steps_json" "gate.failed_step=$failed" "gate.tail=$(tail -n 40 "$log")" \
      gate.result=e0063 phase=gate_failed outcome="gate: E0063 defaults needed (FORK.md section 2)"
    say "gate failed at $failed with E0063/E0027 on FORK.md section 2 fields only; insert the defaults and rerun gate"
    say "report: $S_report"
    return 10
  fi
  st_set "gate.steps:=$steps_json" "gate.failed_step=$failed" "gate.tail=$(tail -n 40 "$log")" \
    gate.result=failed phase=gate_failed outcome=denied
  say "gate failed at $failed; denied (log: $log)"
  say "report: $S_report"
  return 20
}

# --------------------------------------------------------------------------
# land

park() {
  local M=$1 reason=$2 command=$3 ref="refs/fork-sync/$S_run_id"
  git -C "$MAIN" update-ref "$ref" "$M"
  st_set land.landed:=false "land.parked_ref=$ref" "land.reason=$reason" "land.command=$command" \
    phase=parked outcome="merged (landing pending)"
  say "not landed: $reason"
  say "merge parked at $ref ($M)"
  say "landing command: $command"
  return 30
}

cmd_land() {
  acquire_lock
  load_state
  local U=$S_upstream_sha F0=$S_fork_sha M=$S_land_merge_sha
  case "$S_phase" in
    gated | committed | parked | landed | pushed | installed | staged) ;;
    *) die 1 "cannot land from phase ${S_phase:-none}; the gate must pass first" ;;
  esac

  if [[ -z "$M" ]]; then
    check_merge_worktree "$U" "$F0"
    git -C "$WT" add -A
    [[ "$(git -C "$WT" write-tree)" == "$S_gate_gated_tree" ]] ||
      die 1 "the sync worktree changed after the gate; rerun gate"
    local msg
    msg="chore(sync): merge upstream/$UPSTREAM_BRANCH $(git -C "$MAIN" rev-parse --short "$U") ($S_commit_count commits)"
    git -C "$WT" -c commit.gpgsign=false commit --no-verify --quiet -m "$msg"
    M=$(git -C "$WT" rev-parse HEAD)
    st_set "land.merge_sha=$M" land.landed:=false land.pushed:=false phase=committed
    say "committed $M: $msg"
  fi

  local push_cmd="git -C $MAIN push --no-follow-tags $MINE_REMOTE $M:refs/heads/$FORK_BRANCH"
  if is_dry_run; then
    st_set land.dry_run:=true "land.command=git -C $MAIN merge --ff-only $M" \
      "land.push_command=$push_cmd" outcome="merged (dry run: not landed)"
    say "dry run: would run git -C $MAIN merge --ff-only $M"
    say "dry run: would run $push_cmd"
    return 0
  fi

  local cur head_ref
  cur=$(git -C "$MAIN" rev-parse "refs/heads/$FORK_BRANCH")
  if [[ "$cur" != "$M" ]] && ! git -C "$MAIN" merge-base --is-ancestor "$M" "$cur"; then
    head_ref=$(git -C "$MAIN" symbolic-ref --quiet HEAD || true)
    if [[ "$head_ref" != "refs/heads/$FORK_BRANCH" ]]; then
      park "$M" "main checkout is on ${head_ref:-a detached HEAD}, not $FORK_BRANCH" \
        "git -C $MAIN switch $FORK_BRANCH && git -C $MAIN merge --ff-only $M && $SELF land"
      return $?
    fi
    if [[ "$cur" != "$F0" ]]; then
      park "$M" "$FORK_BRANCH moved since prepare (${F0:0:8} -> ${cur:0:8})" "$SELF prepare"
      return $?
    fi
    if [[ -n "$(git -C "$MAIN" status --porcelain --untracked-files=no)" ]]; then
      park "$M" "main checkout has uncommitted changes to tracked files" \
        "git -C $MAIN merge --ff-only $M && $SELF land"
      return $?
    fi
    if ! git -C "$MAIN" merge --ff-only --quiet "$M" >/dev/null; then
      park "$M" "git merge --ff-only refused" "git -C $MAIN merge --ff-only $M && $SELF land"
      return $?
    fi
    say "landed: $FORK_BRANCH fast-forwarded ${F0:0:8} -> ${M:0:8}"
  else
    say "already landed: $FORK_BRANCH contains ${M:0:8}"
  fi

  local unpushed remote_sha
  unpushed=$(git -C "$MAIN" rev-list --count "refs/remotes/$MINE_REMOTE/$FORK_BRANCH..$F0" 2>/dev/null || echo 0)
  st_set land.landed:=true land.parked_ref= land.reason= land.command= \
    "land.unpushed_fork_commits:=$unpushed" phase=landed outcome=merged
  remote_sha=$(git -C "$MAIN" ls-remote "$MINE_REMOTE" "refs/heads/$FORK_BRANCH" 2>/dev/null | cut -f1 || true)
  if [[ -n "$remote_sha" ]] && { [[ "$remote_sha" == "$M" ]] ||
    git -C "$MAIN" merge-base --is-ancestor "$M" "$remote_sha" 2>/dev/null; }; then
    say "already pushed: $MINE_REMOTE/$FORK_BRANCH contains ${M:0:8}"
  elif ! git -C "$MAIN" push --no-follow-tags --quiet "$MINE_REMOTE" "$M:refs/heads/$FORK_BRANCH"; then
    st_set land.pushed:=false "land.push_command=$push_cmd"
    say "push to $MINE_REMOTE failed (never forced); run: $push_cmd"
    return 31
  else
    say "pushed ${M:0:8} to $MINE_REMOTE/$FORK_BRANCH"
  fi
  st_set land.pushed:=true land.push_command= phase=pushed
  say "report: $S_report"
  return 0
}

# --------------------------------------------------------------------------
# install

codesign_file() {
  local -a sign_cmd
  read -r -a sign_cmd <<<"$CODESIGN"
  "${sign_cmd[@]}" "$1" >/dev/null
}

cmd_install() {
  acquire_lock
  load_state
  local M=$S_land_merge_sha
  [[ -n "$M" ]] || die 1 "nothing landed in this run; run land first"
  if is_dry_run; then
    st_set install.result=skipped install.reason="dry run"
    say "dry run: install skipped"
    return 0
  fi
  git -C "$MAIN" merge-base --is-ancestor "$M" "refs/heads/$FORK_BRANCH" ||
    die 1 "${M:0:8} is not on $FORK_BRANCH yet; run land first"
  [[ -f "$BUILT_BINARY" ]] || die 1 "no release build at $BUILT_BINARY; rerun gate"
  [[ "$(git -C "$MAIN" rev-parse "$M^{tree}")" == "$S_gate_gated_tree" ]] ||
    die 1 "the built tree is not the merge tree; rerun gate"

  local baseline=""
  if [[ -s "$STAMP" ]]; then
    baseline=$(tr -d '[:space:]' <"$STAMP")
    git -C "$MAIN" cat-file -e "$baseline^{commit}" 2>/dev/null || baseline=""
  fi
  if [[ "$baseline" == "$M" ]]; then
    st_set install.result=already
    say "already installed: $M"
    return 0
  fi

  local assignments S_protocol_changed="" S_verdict=""
  assignments=$(py restart --repo "$MAIN" --before "$baseline" --after "$M" --shell)
  eval "$assignments"
  mkdir -p "$BIN_DIR" "$STAMP_DIR"

  if [[ -z "$baseline" || "$S_protocol_changed" == true ]]; then
    local reason="protocol or endpoint generation changed"
    [[ -n "$baseline" ]] || reason="no installed-sha stamp at $STAMP; installed protocol unknown"
    cp "$BUILT_BINARY" "$BIN_DIR/herdr.next.tmp"
    codesign_file "$BIN_DIR/herdr.next.tmp"
    mv -f "$BIN_DIR/herdr.next.tmp" "$BIN_DIR/herdr.next"
    local command="mv -f $BIN_DIR/herdr.next $BIN_DIR/herdr && printf '%s\\n' $M > $STAMP"
    st_set 'install:={}' install.result=staged "install.reason=$reason" "install.verdict=$S_verdict" \
      "install.baseline_sha=$baseline" "install.bin_dir=$BIN_DIR" "install.path=$BIN_DIR/herdr.next" \
      "install.command=$command" "install.staged_sha=$M" phase=staged
    say "staged $BIN_DIR/herdr.next ($reason); not installed"
    say "$S_verdict"
    say "when ready, then restart the server: $command"
    return 40
  fi

  cp "$BUILT_BINARY" "$BIN_DIR/herdr.new"
  codesign_file "$BIN_DIR/herdr.new"
  if [[ -e "$BIN_DIR/herdr" ]]; then
    cp -p "$BIN_DIR/herdr" "$BIN_DIR/herdr.prev"
  fi
  mv -f "$BIN_DIR/herdr.new" "$BIN_DIR/herdr"
  printf '%s\n' "$M" >"$STAMP.tmp"
  mv -f "$STAMP.tmp" "$STAMP"
  st_set 'install:={}' install.result=installed "install.verdict=$S_verdict" \
    "install.baseline_sha=$baseline" "install.bin_dir=$BIN_DIR" "install.path=$BIN_DIR/herdr" \
    phase=installed
  say "installed $BIN_DIR/herdr (${M:0:8}); previous kept as herdr.prev"
  say "$S_verdict"
  return 0
}

# --------------------------------------------------------------------------
# abort / note

cmd_abort() {
  acquire_lock
  load_state
  if [[ -n "$S_land_merge_sha" && "$S_land_landed" != true ]]; then
    git -C "$MAIN" update-ref "refs/fork-sync/$S_run_id" "$S_land_merge_sha"
    say "kept the unlanded merge at refs/fork-sync/$S_run_id"
  fi
  git -C "$MAIN" worktree prune
  if is_registered_worktree && [[ -n "$S_fork_sha" ]]; then
    reset_worktree "$S_fork_sha"
    say "dropped the in-progress merge in $WT"
  fi
  if [[ -f "$STATE" ]]; then
    st_set phase=aborted outcome=aborted
    say "report kept: $S_report"
  fi
  return 0
}

cmd_note() {
  [[ $# -gt 0 ]] || die 1 "usage: fork_sync.sh note TEXT"
  acquire_lock
  [[ -f "$STATE" ]] || die 1 "no run in progress"
  st_append notes "$*"
}

main() {
  local cmd=${1:-}
  [[ $# -gt 0 ]] && shift
  case "$cmd" in
    status) cmd_status ;;
    prepare) cmd_prepare ;;
    gate) cmd_gate ;;
    land) cmd_land ;;
    install) cmd_install ;;
    abort) cmd_abort ;;
    note) cmd_note "$@" ;;
    gate-steps)
      [[ -f "$FORK_MD" ]] || die 1 "no FORK.md snapshot; run prepare first"
      gate_steps
      ;;
    *) die 1 "usage: fork_sync.sh <status|prepare|gate|land|install|abort|note TEXT|gate-steps>" ;;
  esac
}

main "$@"
