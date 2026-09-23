#!/usr/bin/env bash
# ABOUTME: Pins consumer-bump.yml's CI gate budgets and its merge-conflict retry, run from the YAML itself
# ABOUTME: A fake clock, scripted run timelines and stand-in gh/git/date/sleep make every scenario deterministic

# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 dravr.ai
#
# Two steps of the reusable workflow are under test, lifted out of the YAML so
# the text that runs here is the text the runner executes:
#
#   "Poll until every gate reaches a terminal status" — the gate has two
#   budgets. GATE_QUEUE is spent while some gate has not started; GATE_TIMEOUT
#   starts only once every gate is running. With a single clock, a busy fleet
#   burned the whole budget in the queue: on 2026-09-22 the platform's and
#   embacle's tronc 1.3.0 bumps timed out with ci-backend still `queued`.
#
#   "Squash merge and delete the branch" — a squash that conflicts because main
#   moved during the gate re-dispatches the caller instead of stalling, capped
#   per day. enforme and equilibre died there the same day, CI green, because
#   their own releases rewrote the line above the tronc pin.
#
# Usage: scripts/consumer-bump-gate.test.sh — needs bash, awk, python3.

set -euo pipefail
cd "$(dirname "$0")/.."
WORKFLOW=".github/workflows/consumer-bump.yml"
WORK=$(mktemp -d); trap 'rm -rf "${WORK}"' EXIT
FAILURES=0
pass() { printf '  ok   %s\n' "$1"; }
fail() { printf '  FAIL %s\n' "$1"; FAILURES=$(( FAILURES + 1 )); }

lift() {  # lift <step name> <out file> <sanity string>
  awk -v name="      - name: $1" '
    $0 == name { in_step = 1; next }
    in_step && /^        run: \|$/ { in_run = 1; next }
    in_run {
      if ($0 ~ /^          / || $0 ~ /^[[:space:]]*$/) { sub(/^          /, ""); print; next }
      exit
    }
  ' "${WORKFLOW}" > "$2"
  grep -q "$3" "$2" || { echo "could not lift '$1' out of ${WORKFLOW} — its name or indentation changed"; exit 1; }
}
GATE="${WORK}/gate.sh";  lift "Poll until every gate reaches a terminal status" "${GATE}" "QUEUE_DEADLINE"
MERGE="${WORK}/merge.sh"; lift "Squash merge and delete the branch" "${MERGE}" "MAX_RETRIES_PER_DAY"

# ---------------------------------------------------------------------------
# Stand-ins. CLOCK holds fake epoch seconds; `sleep` advances it, `date +%s`
# reads it. TIMELINE rows are "<minute> <workflow> <status> <conclusion> [sha]";
# the gate asks for W's runs on one commit (`gh api .../workflows/W/runs?head_sha=S`)
# and gets the latest row for W on S at or before the current minute, or nothing
# (not started) when there is none. A row with no sha is on the commit under test.
# ---------------------------------------------------------------------------
BIN="${WORK}/bin"; mkdir -p "${BIN}"
cat > "${BIN}/date" <<'SH'
#!/usr/bin/env bash
case "$*" in
  "+%s") cat "${CLOCK}" ;;
  *"24 hours ago"*) echo "2026-09-21T00:00:00Z" ;;
  *) /bin/date "$@" ;;
esac
SH
cat > "${BIN}/sleep" <<'SH'
#!/usr/bin/env bash
echo $(( $(cat "${CLOCK}") + $1 )) > "${CLOCK}"
SH
cat > "${BIN}/gh" <<'SH'
#!/usr/bin/env bash
echo "gh $*" >> "${CALLS}"
if [ "$1" = "api" ] && [[ "$2" == *"/actions/workflows/"*"/runs?head_sha="* ]]; then
  wf="${2#*/actions/workflows/}"; wf="${wf%%/runs*}"
  sha="${2#*head_sha=}"; sha="${sha%%&*}"
  now_min=$(( ( $(cat "${CLOCK}") - START ) / 60 ))
  awk -v wf="${wf}" -v now="${now_min}" -v sha="${sha}" -v head="${HEAD_SHA}" '
    { row_sha = (NF >= 5) ? $5 : head }
    $2 == wf && $1 <= now && row_sha == sha { line = $3 " " $4 " 4242" }
    END { if (line) print line }' "${TIMELINE}"
elif [ "$1" = "api" ] && [[ "$2" == *"/actions/runs/4242/jobs"* ]]; then
  # The jobs of a finished run, filtered by the step to those that concluded
  # failure with no step executed — GitHub's "not started" shape.
  printf '%s\n' "${UNSTARTED_JOBS:-}"
elif [ "$1 $2" = "run list" ] && [[ " $* " == *" --branch "* ]]; then
  # GitHub's branch listing: the newest run on the branch, whatever commit it was
  # on — which is exactly what a gate matching by branch would read.
  wf=""; prev=""
  for a in "$@"; do [ "${prev}" = "--workflow" ] && wf="$a"; prev="$a"; done
  now_min=$(( ( $(cat "${CLOCK}") - START ) / 60 ))
  awk -v wf="${wf}" -v now="${now_min}" '$2 == wf && $1 <= now { line = $3 " " $4 } END { if (line) print line }' "${TIMELINE}"
elif [ "$1 $2" = "run list" ]; then
  echo "${RECENT_RUNS}"
elif [ "$1 $2" = "workflow run" ]; then
  :
fi
SH
cat > "${BIN}/git" <<'SH'
#!/usr/bin/env bash
echo "git $*" >> "${CALLS}"
case "$1 $2" in
  "merge --squash") exit "${MERGE_RC:-0}" ;;
esac
exit 0
SH
chmod +x "${BIN}"/*

START=1790000000
HEAD_SHA="c0ffee0000000000000000000000000000000000"
run_gate() {  # run_gate <queue min> <run min> <workflows> <timeline rows...>
  local queue="$1" budget="$2" wfs="$3"; shift 3
  TIMELINE="${WORK}/timeline"; printf '%s\n' "$@" > "${TIMELINE}"
  CLOCK="${WORK}/clock"; echo "${START}" > "${CLOCK}"
  CALLS="${WORK}/calls"; : > "${CALLS}"
  set +e
  GH_OUTPUT_FILE="${WORK}/gh_output"; : > "${GH_OUTPUT_FILE}"
  OUT=$(PATH="${BIN}:${PATH}" CLOCK="${CLOCK}" START="${START}" TIMELINE="${TIMELINE}" CALLS="${CALLS}" \
        GITHUB_OUTPUT="${GH_OUTPUT_FILE}" UNSTARTED_JOBS="${UNSTARTED_JOBS:-}" \
        BRANCH="fix/tronc-9.9.9" SHA="${HEAD_SHA}" HEAD_SHA="${HEAD_SHA}" GH_REPO="dravr-ai/dravr-x" \
        CI_WORKFLOWS="${wfs}" GATE_TIMEOUT="${budget}" GATE_QUEUE="${queue}" \
        bash "${GATE}" 2>&1)
  RC=$?
  set -e
  ELAPSED=$(( ( $(cat "${CLOCK}") - START ) / 60 ))
}
check() {  # check <name> <expected rc> <expected output fragment>
  if [ "${RC}" = "$2" ] && grep -qF -- "$3" <<<"${OUT}"; then pass "$1"
  else fail "$1 (rc=${RC}, want $2; output did not contain '$3')"; printf '%s\n' "${OUT}" | tail -3 | sed 's/^/       /'; fi
}

echo "gate budgets"
run_gate 180 45 "ci.yml" "0 ci.yml queued -" "170 ci.yml in_progress -" "200 ci.yml completed success"
check "170m queued then 30m running is green — the 2026-09-22 platform case" 0 "all gates green"

run_gate 180 45 "ci.yml" "0 ci.yml queued -" "40 ci.yml in_progress -" "50 ci.yml completed success"
check "40m queue + 10m run passes a 45m run budget (one clock would have failed it)" 0 "all gates green"

run_gate 180 45 "ci.yml" "0 ci.yml queued -"
check "never picked up within the queue budget fails, naming the queue" 1 "no runner picked up"
[ "${ELAPSED}" -ge 180 ] && [ "${ELAPSED}" -le 182 ] && pass "  ...and only after the full 180m" || fail "  queue refusal came at ${ELAPSED}m"

run_gate 180 45 "ci.yml" "0 ci.yml in_progress -"
check "running past the run budget fails, naming the run budget" 1 "of running"
[ "${ELAPSED}" -ge 45 ] && [ "${ELAPSED}" -le 47 ] && pass "  ...at 45m of running" || fail "  run refusal came at ${ELAPSED}m"

run_gate 180 45 "ci.yml" "0 ci.yml in_progress -" "12 ci.yml completed failure"
check "a red gate fails at once, not at a deadline" 1 "CI is not green"

run_gate 180 45 "a.yml b.yml" "0 a.yml in_progress -" "0 b.yml queued -" "100 b.yml in_progress -" "120 a.yml completed success" "130 b.yml completed success"
check "the run budget starts only when the LAST gate starts" 0 "all gates green"
grep -q "run budget starts now" <<<"${OUT}" && pass "  ...and says when it started" || fail "  never announced the run budget"

run_gate 180 45 "ci.yml"
check "a gate that never produces a run is waiting, not running" 1 "no runner picked up"

run_gate 180 45 "ci.yml" "0 ci.yml completed success deadbeef00000000000000000000000000000000"
check "a green run from a previous attempt on another commit does not satisfy the gate" 1 "no runner picked up"

run_gate 180 45 "ci.yml" "0 ci.yml completed success deadbeef00000000000000000000000000000000" "5 ci.yml in_progress -" "20 ci.yml completed failure"
check "  ...and this commit's own red is what decides, not the stale green" 1 "CI is not green"

UNSTARTED_JOBS="Architectural Validation"
run_gate 180 45 "ci.yml" "0 ci.yml in_progress -" "10 ci.yml completed failure"
check "a job GitHub never started is named as a billing refusal, not a red tree" 1 "did not start jobs"
grep -q "verdict=not-started" "${GH_OUTPUT_FILE}" && pass "  ...and hands the stall report verdict=not-started" || fail "  verdict not recorded as not-started"
UNSTARTED_JOBS=""

run_gate 180 45 "ci.yml" "0 ci.yml in_progress -" "10 ci.yml completed failure"
check "a job that ran and failed is still a red tree" 1 "CI is not green"
grep -q "verdict=red" "${GH_OUTPUT_FILE}" && pass "  ...and hands the stall report verdict=red" || fail "  verdict not recorded as red"

# ---------------------------------------------------------------------------
echo "merge retry"
run_merge() {  # run_merge <merge rc> <recent runs>
  CALLS="${WORK}/calls"; : > "${CALLS}"; SUMMARY="${WORK}/summary"; : > "${SUMMARY}"
  set +e
  OUT=$(PATH="${BIN}:${PATH}" CALLS="${CALLS}" MERGE_RC="$1" RECENT_RUNS="$2" \
        BRANCH="fix/tronc-9.9.9" VERSION="9.9.9" PINNED="9.9.8" GH_TOKEN=x GH_REPO=dravr-ai/dravr-x \
        CALLER_WORKFLOW_REF="dravr-ai/dravr-x/.github/workflows/tronc-bump.yml@refs/heads/main" \
        MAX_RETRIES_PER_DAY=4 GITHUB_STEP_SUMMARY="${SUMMARY}" CLOCK="${WORK}/clock" \
        bash "${MERGE}" 2>&1)
  RC=$?
  set -e
}
called() { grep -qF -- "$1" "${CALLS}"; }

run_merge 0 1
[ "${RC}" = 0 ] && called "git commit -m chore(deps): bump dravr-tronc 9.9.8 -> 9.9.9" && called "git push origin main" \
  && called "git push origin --delete fix/tronc-9.9.9" && ! called "gh workflow run" \
  && pass "a clean squash commits, pushes main and deletes the branch — no retry" || fail "clean squash (rc=${RC})"

run_merge 1 1
[ "${RC}" = 0 ] && called "git merge --abort" && called "gh workflow run tronc-bump.yml --ref main -f version=9.9.9" \
  && ! called "git push origin main" && grep -q "Re-dispatched" "${SUMMARY}" \
  && pass "a conflicting squash aborts and re-dispatches the caller's own workflow, pushing nothing" || fail "conflict retry (rc=${RC})"

run_merge 1 5
[ "${RC}" = 1 ] && ! called "gh workflow run" && grep -q "over the retry cap" <<<"${OUT}" \
  && pass "past the daily cap it fails (so a stall is filed) instead of looping" || fail "retry cap (rc=${RC})"

echo
[ "${FAILURES}" -eq 0 ] && { echo "all consumer-bump gate/merge scenarios pass"; exit 0; }
echo "${FAILURES} scenario(s) failed"; exit 1
