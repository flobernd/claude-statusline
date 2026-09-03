#!/usr/bin/env bash
# Build scratch git states and capture real statusline output for the SVG
# previews. Everything stays under this folder (out/ and work/, both
# gitignored). The captures run against scratch HOME directories so the
# machine's real statusline config, account file, and usage cache can never
# leak into the previews; the transcript lives inside each scratch HOME
# because the transcript reader only trusts paths under ~/.claude.
set -euo pipefail

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BIN=$HERE/../../target/release/claude-statusline
OUT=$HERE/out
WORK=$HERE/work
PHOME=$WORK/home
UHOME=$WORK/home-usage

rm -rf "$OUT" "$WORK"
# dev/notes has to exist on disk: the statusline paints a directory that is
# gone as a dead location, and this capture stands for the healthy chip a
# plain directory outside any repository gets.
mkdir -p "$OUT" "$PHOME/.claude" "$PHOME/dev/notes" "$UHOME/.claude"

# Hermetic git so the scratch repos are deterministic and never touch the
# user's config.
g() { git -C "$1" -c init.defaultBranch=main -c user.name=dev -c user.email=dev@example.com -c commit.gpgsign=false -c advice.detachedHead=false "${@:2}"; }

# Everything time-dependent runs off one pinned instant, 2026-07-24T00:00:00Z.
# A fixed date rather than "now" keeps calendar-derived output stable (the
# spend reset renders the distance to the next 1st of the month), and the
# binary reads the same instant via CLAUDE_STATUSLINE_NOW_MS, so captures are
# byte-identical on any machine, any day. TZ is pinned because the spend
# reset boundary is computed in local time.
export TZ=UTC
NOW_S=1784851200
NOW_MS=$((NOW_S * 1000))
# Byte-exact regeneration must not depend on machine speed: a slow runner can
# blow the default git budget and silently drop branch/ahead-behind/worktree
# chips, capturing wrong SVGs instead of failing loudly.
export CLAUDE_STATUSLINE_GIT_TIMEOUT_MS=10000

# --- transcripts: one assistant message ~72s ago -> cache_age chip ---------
TS=$(date -u -d "@$((NOW_S - 72))" +%Y-%m-%dT%H:%M:%SZ)
for h in "$PHOME" "$UHOME"; do
  printf '{"type":"assistant","timestamp":"%s","message":{"role":"assistant"}}\n' "$TS" > "$h/.claude/t.jsonl"
done

# --- usage HOME: opt-in config, login identity, seeded usage snapshot ------
# The line renders from this snapshot plus the payload rate_limits. The
# next-at stamps sit half an hour ahead, so the capture spawns no real fetch
# child, and under the hour past which a stamp reads as due again. Interval 0
# would drop the cache instead of reading it.
cat > "$UHOME/.claude/claude-statusline.json" <<'JSON'
{"advanced_usage_limits_enabled": true, "usage_fetch_interval_seconds": 60}
JSON
cat > "$UHOME/.claude.json" <<'JSON'
{"oauthAccount": {"accountUuid": "acct-preview"}}
JSON
PARKED_MS=$((NOW_MS + 1800000))
FABLE_RESET=$(date -u -d "@$((NOW_S + 432000))" +%Y-%m-%dT%H:%M:%SZ)
cat > "$UHOME/.claude/claude-statusline-usage.json" <<JSON
{
  "fetched_at_ms": $NOW_MS,
  "account_uuid": "acct-preview",
  "profile": {"email": "user@example.com", "plan": "max", "tier": "default_claude_max_20x"},
  "profile_fetched_at_ms": $NOW_MS,
  "usage_next_at_ms": $PARKED_MS,
  "profile_next_at_ms": $PARKED_MS,
  "utilization": {
    "extra_usage": {"is_enabled": true, "monthly_limit": 100000, "used_credits": 100200, "utilization": 100.2},
    "limits": [
      {"kind": "weekly_scoped", "group": "model", "percent": 81, "resets_at": "$FABLE_RESET",
       "scope": {"model": {"display_name": "Fable"}}}
    ]
  }
}
JSON

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
  "transcript_path": "$1/.claude/t.jsonl",
  "context_window": {
    "used_percentage": 47, "context_window_size": 1000000,
    "current_usage": {"input_tokens": 5200, "cache_creation_input_tokens": 11800, "cache_read_input_tokens": 372000}
  }
JSON
}

cat > "$OUT/p-cwd.json" <<JSON
{ $(common_line1 "$PHOME"),
  "cwd": "$PHOME/dev/notes",
  "workspace": {"current_dir": "$PHOME/dev/notes"}
}
JSON

cat > "$OUT/p-repo.json" <<JSON
{ $(common_line1 "$PHOME"),
  "cwd": "$REPO",
  "workspace": {"current_dir": "$REPO", "repo": {"host": "github.com", "owner": "acme", "name": "webapp"}},
  "pr": {"number": 128, "url": "https://github.com/acme/webapp/pull/128", "review_state": "approved"}
}
JSON

cat > "$OUT/p-worktree.json" <<JSON
{ $(common_line1 "$PHOME"),
  "cwd": "$WT",
  "workspace": {"current_dir": "$WT", "git_worktree": "hotfix", "repo": {"host": "github.com", "owner": "acme", "name": "webapp"}}
}
JSON

# Session + weekly ride the payload like a live session; fable and spend
# come from the seeded snapshot in UHOME.
cat > "$OUT/p-usage.json" <<JSON
{ $(common_line1 "$UHOME"),
  "cwd": "$REPO",
  "workspace": {"current_dir": "$REPO", "repo": {"host": "github.com", "owner": "acme", "name": "webapp"}},
  "pr": {"number": 128, "url": "https://github.com/acme/webapp/pull/128", "review_state": "approved"},
  "rate_limits": {
    "five_hour": {"used_percentage": 42, "resets_at": $((NOW_S + 7800))},
    "seven_day": {"used_percentage": 63, "resets_at": $((NOW_S + 259200))}
  }
}
JSON

cat > "$OUT/p-subagent.json" <<JSON
{
  "columns": 220,
  "cwd": "$REPO",
  "tasks": [
    {"id":"t1","type":"local_agent","name":"Explore","label":"Searching for callers","startTime": $((NOW_MS - 83000)), "model":"claude-sonnet-5","contextWindowSize":200000,"tokenCount":82000},
    {"id":"t2","type":"local_agent","name":"Plan","label":"Drafting the migration steps","startTime": $((NOW_MS - 23000)), "model":"claude-opus-4-8","effort":"high","contextWindowSize":200000,"tokenCount":45000},
    {"id":"t3","type":"local_agent","name":"Explore","label":"Auditing the hotfix","startTime": $((NOW_MS - 41000)), "model":"claude-sonnet-5","contextWindowSize":200000,"tokenCount":58000,"cwd":"$WT"}
  ]
}
JSON

# --- capture real output --------------------------------------------------
# work/ sits inside this repository, so git discovery would climb out of the
# scratch tree and resolve the captures against claude-statusline itself.
export GIT_CEILING_DIRECTORIES=$WORK
export FORCE_COLOR=1 CLAUDE_STATUSLINE_WIDTH=220 CLAUDE_STATUSLINE_NOW_MS=$NOW_MS
unset NO_COLOR || true
for v in cwd repo worktree; do
  HOME=$PHOME "$BIN" < "$OUT/p-$v.json" > "$OUT/main-$v.ansi"
done
HOME=$UHOME "$BIN" < "$OUT/p-usage.json" > "$OUT/main-usage.ansi"
# The subagent capture also runs against scratch state: session cwd and HOME
# point at the scratch clone and home, so location chips and config never
# depend on this machine's real checkout or settings.
HOME=$PHOME "$BIN" --subagent-statusline < "$OUT/p-subagent.json" > "$OUT/subagent.ndjson"

echo "=== main-cwd ===";      cat -v "$OUT/main-cwd.ansi"
echo "=== main-repo ===";     cat -v "$OUT/main-repo.ansi"
echo "=== main-worktree ==="; cat -v "$OUT/main-worktree.ansi"
echo "=== main-usage ===";    cat -v "$OUT/main-usage.ansi"
echo "=== subagent (ndjson) ==="; cat -v "$OUT/subagent.ndjson"
echo "=== done ==="
