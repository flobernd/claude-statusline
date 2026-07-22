#!/usr/bin/env bash
# Build scratch git states and capture real statusline output for the SVG
# previews. Everything stays under this folder (out/ and work/, both
# gitignored); a temporary transcript under ~/.claude is created (required:
# the transcript reader only trusts that root) and removed at the end.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=$HERE/../../target/release/claude-statusline
OUT=$HERE/out
WORK=$HERE/work
TSDIR=$HOME/.claude/.statusline-preview-tmp

rm -rf "$OUT" "$WORK" "$TSDIR"
mkdir -p "$OUT" "$WORK" "$TSDIR"

# Hermetic git so the scratch repos are deterministic and never touch the
# user's config.
g() { git -C "$1" -c init.defaultBranch=main -c user.name=dev -c user.email=dev@example.com -c commit.gpgsign=false -c advice.detachedHead=false "${@:2}"; }

# --- temporary transcript: one assistant message ~72s ago -> cache_age chip ---
TS=$(date -u -d '72 seconds ago' +%Y-%m-%dT%H:%M:%SZ)
printf '{"type":"assistant","timestamp":"%s","message":{"role":"assistant"}}\n' "$TS" > "$TSDIR/t.jsonl"
TRANSCRIPT="$TSDIR/t.jsonl"

# --- repo variant: origin + clone, branch, dirty tree, stashes, ahead/behind ---
ORIGIN=$WORK/origin
REPO=$WORK/webapp
mkdir -p "$ORIGIN"; g "$ORIGIN" init -q
for f in app.js server.js router.js db.js utils.js README.md; do echo "line" > "$ORIGIN/$f"; done
g "$ORIGIN" add -A; g "$ORIGIN" commit -qm "Initial commit"
git -c init.defaultBranch=main clone -q "$ORIGIN" "$REPO"
git -C "$REPO" config user.name dev; git -C "$REPO" config user.email dev@example.com; git -C "$REPO" config commit.gpgsign false
# origin gains a commit the clone hasn't merged -> behind 1
echo "upstream change" >> "$ORIGIN/server.js"; g "$ORIGIN" commit -qam "Tune server timeouts"
g "$REPO" fetch -q origin
# two local commits on main (already tracks origin/main) -> ahead 2, behind 1
echo "a" >> "$REPO/router.js"; g "$REPO" commit -qam "Extract auth middleware"
echo "b" >> "$REPO/db.js";     g "$REPO" commit -qam "Add token store"
# two stashes
echo "wip1" >> "$REPO/app.js"; g "$REPO" stash push -q -m wip1
echo "wip2" >> "$REPO/utils.js"; g "$REPO" stash push -q -m wip2
# visible dirty tree: 3 new, 1 deleted, 2 modified
echo x > "$REPO/login.js"; echo x > "$REPO/session.js"; echo x > "$REPO/oauth.js"
rm "$REPO/utils.js"
echo "mod" >> "$REPO/app.js"; echo "mod" >> "$REPO/router.js"

# --- worktree variant: linked worktree off the same repo ---
WT=$WORK/webapp-hotfix
g "$REPO" worktree add -q "$WT" -b hotfix/1.2
echo "patch" >> "$WT/server.js"; echo "patch" >> "$WT/db.js"

# --- payloads -------------------------------------------------------------
common_line1() {
  cat <<JSON
  "model": {"display_name": "Opus 4.8"},
  "effort": {"level": "high"},
  "transcript_path": "$TRANSCRIPT",
  "context_window": {
    "used_percentage": 47, "context_window_size": 1000000,
    "current_usage": {"input_tokens": 5200, "cache_creation_input_tokens": 11800, "cache_read_input_tokens": 372000}
  }
JSON
}

cat > "$OUT/p-cwd.json" <<JSON
{ $(common_line1),
  "cwd": "$HOME/dev/notes",
  "workspace": {"current_dir": "$HOME/dev/notes"}
}
JSON

cat > "$OUT/p-repo.json" <<JSON
{ $(common_line1),
  "cwd": "$REPO",
  "workspace": {"current_dir": "$REPO", "repo": {"host": "github.com", "owner": "acme", "name": "webapp"}},
  "pr": {"number": 128, "url": "https://github.com/acme/webapp/pull/128", "review_state": "approved"}
}
JSON

cat > "$OUT/p-worktree.json" <<JSON
{ $(common_line1),
  "cwd": "$WT",
  "workspace": {"current_dir": "$WT", "git_worktree": "hotfix", "repo": {"host": "github.com", "owner": "acme", "name": "webapp"}}
}
JSON

NOW=$(date +%s%3N)
cat > "$OUT/p-subagent.json" <<JSON
{
  "columns": 220,
  "cwd": "$HOME/workspace/claude-statusline",
  "tasks": [
    {"id":"t1","type":"local_agent","name":"Explore","label":"Searching for callers","startTime": $((NOW-83000)), "model":"claude-sonnet-5","contextWindowSize":200000,"tokenCount":82000},
    {"id":"t2","type":"local_agent","name":"Plan","label":"Drafting the migration steps","startTime": $((NOW-23000)), "model":"claude-opus-4-8","effort":"high","contextWindowSize":200000,"tokenCount":45000},
    {"id":"t3","type":"local_agent","name":"Explore","label":"Auditing the hotfix","startTime": $((NOW-41000)), "model":"claude-sonnet-5","contextWindowSize":200000,"tokenCount":58000,"cwd":"$WT"}
  ]
}
JSON

# --- capture real output --------------------------------------------------
export FORCE_COLOR=1 CLAUDE_STATUSLINE_WIDTH=220
unset NO_COLOR || true
for v in cwd repo worktree; do
  "$BIN" < "$OUT/p-$v.json" > "$OUT/main-$v.ansi"
done
"$BIN" --subagent-statusline < "$OUT/p-subagent.json" > "$OUT/subagent.ndjson"

echo "=== main-cwd ===";      cat -v "$OUT/main-cwd.ansi"
echo "=== main-repo ===";     cat -v "$OUT/main-repo.ansi"
echo "=== main-worktree ==="; cat -v "$OUT/main-worktree.ansi"
echo "=== subagent (ndjson) ==="; cat -v "$OUT/subagent.ndjson"

rm -rf "$TSDIR"
echo "=== done; transcript tmp removed ==="
