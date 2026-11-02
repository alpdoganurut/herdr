#!/usr/bin/env bash
# Install the herdr+ Claude Code skills (plus:notes, plus:notify, plus:status,
# plus:team-build) from docs/fork/skills into the user's Claude skills folder.
#
# Usage: scripts/install-plus-skills.sh [--dest <skills-dir>] [--dry-run]
#   --dest     target folder (default: ${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills)
#   --dry-run  print what would change, touch nothing
#
# Idempotent: a skill already identical to the repo copy is left alone. A skill
# that differs is moved to <dest>/../skills-backup/<name>.<timestamp> first (kept
# outside the skills folder so Claude Code does not load the backup as a skill).
# The repo stores each skill as plus-<name> (no ':' in tracked paths); it is
# installed as plus:<name>, the name its SKILL.md declares.
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
src_root="$repo/docs/fork/skills"
dest="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills"
dry_run=0

while [ $# -gt 0 ]; do
  case "$1" in
    --dest)
      [ $# -ge 2 ] || { echo "install-plus-skills: --dest needs a folder" >&2; exit 2; }
      dest=$2; shift 2 ;;
    --dry-run) dry_run=1; shift ;;
    -h|--help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "install-plus-skills: unknown argument: $1" >&2; exit 2 ;;
  esac
done

[ -d "$src_root" ] || { echo "install-plus-skills: missing $src_root" >&2; exit 1; }

backup_root="$(dirname "$dest")/skills-backup"
stamp=$(date +%Y%m%d-%H%M%S)
installed=0; unchanged=0; backed_up=0

[ "$dry_run" = 1 ] || mkdir -p "$dest"

for src in "$src_root"/plus-*/; do
  src=${src%/}
  [ -f "$src/SKILL.md" ] || continue
  base=$(basename "$src")
  name="plus:${base#plus-}"
  target="$dest/$name"

  if [ -d "$target" ] && diff -rq -x '.DS_Store' "$src" "$target" >/dev/null 2>&1; then
    echo "up to date  $name"
    unchanged=$((unchanged + 1))
    continue
  fi

  if [ -e "$target" ]; then
    backup="$backup_root/$name.$stamp"
    echo "back up     $name -> $backup"
    if [ "$dry_run" = 0 ]; then
      mkdir -p "$backup_root"
      mv "$target" "$backup"
    fi
    backed_up=$((backed_up + 1))
  fi

  echo "install     $name -> $target"
  if [ "$dry_run" = 0 ]; then
    tmp="$dest/.$name.installing.$$"
    rm -rf "$tmp"
    cp -R "$src" "$tmp"
    find "$tmp" -name '*.sh' -exec chmod +x {} +
    mv "$tmp" "$target"
  fi
  installed=$((installed + 1))
done

echo "installed $installed, unchanged $unchanged, backed up $backed_up (dest $dest)"
[ "$installed" = 0 ] || [ "$dry_run" = 1 ] || echo "restart Claude Code (or start a new session) to load the skills"
