#!/usr/bin/env bash
# ABOUTME: Pins the lockstep behaviour of consumer-bump.yml's "Rewrite every dravr-tronc pin" step
# ABOUTME: Runs the step body verbatim against fixture manifests, with curl/git/cargo replaced by stand-ins on PATH

# SPDX-License-Identifier: MIT OR Apache-2.0
# Copyright (c) 2026 dravr.ai
#
# The step under test is inline bash in a reusable workflow, which only ever runs
# on GitHub, inside a consumer's checkout, against crates.io and the consumers'
# private remotes. Nothing about it can be exercised by `cargo test`. So this
# script lifts the step body out of the YAML — the same text the runner executes,
# not a copy that drifts — and runs it in a throwaway repo shaped like the
# platform's manifests, with every network-facing command on PATH answered from
# canned fixtures. The resolver is canned too: `cargo update` here copies in the
# lockfile a real resolve would have produced, because what is under test is the
# step's own assertions about that lockfile, not Cargo's.
#
# What it proves, one scenario each:
#   - a `git:<crate>` entry moves the tag on every pin line to the highest vX.Y.Z
#     tag the remote carries, sorted as versions, skipping non-version tags —
#     and reads that tag's own tronc requirement out of a shallow clone made
#     from the runner's scratch directory, never from inside the checkout
#   - a `git:` entry whose latest tag pins an older tronc is refused, naming the
#     crate and the tag to cut, before anything of it is rewritten
#   - a `git:` entry with no vX.Y.Z tag at all is refused before any clone
#   - a `git:` entry no manifest pins is refused before any remote is asked
#   - a tag whose manifest pins tronc by git tag rather than by version is read
#     the same way
#   - a bare crates.io entry behaves exactly as it did before the git arm existed,
#     and never touches git
#   - a lockfile that resolves the git crate at the wrong tag fails the assertion
#
# The step runs under `set -euo pipefail`, so every `x=$(a | b)` in it ends the
# step on the pipeline's status unless the arm carries `|| true`; the refusal
# scenarios below are the ones that exercise those arms.
#
# Usage: scripts/consumer-bump-lockstep.test.sh
# Needs bash, jq, awk, sed, sort -V; no network, no cargo. GNU sed is what the
# runner has; on a BSD sed the in-place flag is bridged, nothing else differs.

set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORKFLOW="${ROOT}/.github/workflows/consumer-bump.yml"
WORK=$(mktemp -d)
trap 'rm -rf "${WORK}"' EXIT

FAILURES=0
pass() { printf '  ok   %s\n' "$1"; }
fail() { printf '  FAIL %s\n' "$1"; FAILURES=$(( FAILURES + 1 )); }

# ---------------------------------------------------------------------------
# The step body, lifted from the YAML. A `run: |` block scalar drops its common
# indentation, ten spaces here; this does the same. The sanity grep is what
# makes a re-indented workflow fail loudly instead of testing an empty script.
# ---------------------------------------------------------------------------
STEP="${WORK}/rewrite-step.sh"
awk '
  /^      - name: Rewrite every dravr-tronc pin$/ { in_step = 1; next }
  in_step && /^        run: \|$/ { in_run = 1; next }
  in_run {
    if ($0 ~ /^          / || $0 ~ /^[[:space:]]*$/) { sub(/^          /, ""); print; next }
    exit
  }
' "${WORKFLOW}" > "${STEP}"
if ! grep -q 'cargo update -p dravr-tronc' "${STEP}"; then
  echo "could not lift the rewrite step out of ${WORKFLOW} — its name or indentation changed"
  exit 1
fi

# ---------------------------------------------------------------------------
# Stand-ins for the commands the step reaches out with. Each one logs its
# invocation to CALLS and answers from the FIXTURE tree the scenario points at.
# ---------------------------------------------------------------------------
STUBS="${WORK}/bin"
mkdir -p "${STUBS}"

cat > "${STUBS}/curl" <<'EOF'
#!/usr/bin/env bash
# crates.io, as the step calls it: `curl -sf -A UA <url>`. Answers from
# FIXTURE/crates-io/<crate>.json and <crate>-<version>-deps.json; a missing
# file is curl -f's exit 22 on a 404.
url="${*: -1}"
echo "curl ${url}" >> "${CALLS}"
case "${url}" in
  https://crates.io/api/v1/crates/*/*/dependencies)
    rest="${url#https://crates.io/api/v1/crates/}"
    crate="${rest%%/*}"; ver="${rest#*/}"; ver="${ver%/dependencies}"
    f="${FIXTURE}/crates-io/${crate}-${ver}-deps.json" ;;
  https://crates.io/api/v1/crates/*)
    f="${FIXTURE}/crates-io/${url#https://crates.io/api/v1/crates/}.json" ;;
  *) exit 22 ;;
esac
[ -f "${f}" ] || exit 22
cat "${f}"
EOF

cat > "${STUBS}/git" <<'EOF'
#!/usr/bin/env bash
# The two calls the git arm makes, both from the runner's scratch directory:
#   git ls-remote --tags <url>
#     answered from FIXTURE/remote/<owner>/<repo>/tags.txt
#   git clone --quiet --depth 1 --branch <tag> <url> <dir>
#     answered by placing FIXTURE/remote/<owner>/<repo>/<tag>/Cargo.toml at
#     <dir>/Cargo.toml — the only file the step reads out of the clone
# Anything else is a step reaching for git in a way this harness does not know.
echo "git $*" >> "${CALLS}"
case "$1 $2" in
  "ls-remote --tags")
    url="$3"
    ownerrepo="${url#https://github.com/}"; ownerrepo="${ownerrepo%.git}"
    f="${FIXTURE}/remote/${ownerrepo}/tags.txt"
    [ -f "${f}" ] || { echo "fatal: repository '${url}' not found" >&2; exit 128; }
    cat "${f}"
    ;;
  "clone --quiet")
    tag=""; url=""; dir=""
    shift 2
    while [ $# -gt 0 ]; do
      case "$1" in
        --depth) shift ;;
        --branch) tag="$2"; shift ;;
        *) if [ -z "${url}" ]; then url="$1"; else dir="$1"; fi ;;
      esac
      shift
    done
    ownerrepo="${url#https://github.com/}"; ownerrepo="${ownerrepo%.git}"
    f="${FIXTURE}/remote/${ownerrepo}/${tag}/Cargo.toml"
    [ -f "${f}" ] || { echo "fatal: Remote branch ${tag} not found in upstream origin" >&2; exit 128; }
    mkdir -p "${dir}" && cp "${f}" "${dir}/Cargo.toml"
    ;;
  *) echo "git stand-in: unexpected: $*" >&2; exit 1 ;;
esac
EOF

cat > "${STUBS}/cargo" <<'EOF'
#!/usr/bin/env bash
# `cargo update -p ...`: the resolve is canned. The scenario supplies the lock
# a real resolve of its manifests would write, as Cargo.lock.resolved.
echo "cargo $*" >> "${CALLS}"
[ "$1" = "update" ] || { echo "cargo stand-in: only update is answered, got: $*" >&2; exit 1; }
[ -f Cargo.lock.resolved ] && cp Cargo.lock.resolved Cargo.lock
exit 0
EOF

# The runner's sed is GNU, whose `-i` takes no argument. BSD sed wants `-i ''`
# for the same thing; bridge only that flag, so the step text stays untouched.
if ! sed --version 2>/dev/null | grep -q GNU; then
  if command -v gsed >/dev/null 2>&1; then
    printf '#!/usr/bin/env bash\nexec gsed "$@"\n' > "${STUBS}/sed"
  else
    cat > "${STUBS}/sed" <<'EOF'
#!/usr/bin/env bash
args=()
for a in "$@"; do
  if [ "${a}" = "-i" ]; then args+=(-i ''); else args+=("${a}"); fi
done
exec /usr/bin/sed "${args[@]}"
EOF
  fi
fi
chmod +x "${STUBS}"/*

# ---------------------------------------------------------------------------
# Fixtures: what the network would have said.
# ---------------------------------------------------------------------------
FIX="${WORK}/fixture"
mkdir -p "${FIX}/crates-io" "${FIX}/remote/dravr-ai/dravr-stripe/v0.1.14" \
         "${FIX}/remote/dravr-ai/dravr-stripe/v0.1.13" "${FIX}/remote/dravr-ai/by-tag/v0.3.0"

# embacle-tool-host: latest 0.25.0 (a yanked 0.25.1 above it, which must be
# skipped), and 0.25.0 requires ^1.0.0 — the real shape on 2026-09-04.
cat > "${FIX}/crates-io/embacle-tool-host.json" <<'EOF'
{"versions":[{"num":"0.25.1","yanked":true},{"num":"0.25.0","yanked":false},{"num":"0.24.0","yanked":false}]}
EOF
cat > "${FIX}/crates-io/embacle-tool-host-0.25.0-deps.json" <<'EOF'
{"dependencies":[{"crate_id":"serde","req":"^1.0"},{"crate_id":"dravr-tronc","req":"^1.0.0"}]}
EOF

# dravr-stripe's tags, as ls-remote prints them: unsorted, with peeled `^{}`
# refs for the annotated ones and two tags that are not releases at all.
# v0.1.9 is there because it sorts ABOVE v0.1.14 as text.
cat > "${FIX}/remote/dravr-ai/dravr-stripe/tags.txt" <<'EOF'
ee207037f6320aba244435b0eb7eb5df381da612	refs/tags/v0.1.1
95e38fd2ddaae441fde424430db8c50ccaae6d96	refs/tags/v0.1.10
25e4cbd682a899bd1abfc1c16f4f1f0cba3c53cb	refs/tags/v0.1.13
afbd8b1b67cd4b48f21b4d9bd2ca16641d0c0fff	refs/tags/v0.1.14
a238324e3eef33998c2886ec315409b080c01334	refs/tags/v0.1.3
62357916463cf23f0e353379c518272775aa2806	refs/tags/v0.1.3^{}
37e3bf0df5c1e3544738e1474c8daa5d0e2a6322	refs/tags/v0.1.9
0000000000000000000000000000000000000001	refs/tags/nightly
0000000000000000000000000000000000000002	refs/tags/v2
EOF
cat > "${FIX}/remote/dravr-ai/dravr-stripe/v0.1.14/Cargo.toml" <<'EOF'
[package]
name = "dravr-stripe"
version = "0.1.14"

[dependencies]
serde = "1.0"
dravr-tronc = { version = "1.0.0", features = ["notifications"] }
EOF
cat > "${FIX}/remote/dravr-ai/dravr-stripe/v0.1.13/Cargo.toml" <<'EOF'
[package]
name = "dravr-stripe"
version = "0.1.13"

[dependencies]
dravr-tronc = { version = "0.11.0", features = ["notifications"] }
EOF

# A remote with no release tag at all: a two-component tag is not vX.Y.Z either.
mkdir -p "${FIX}/remote/dravr-ai/untagged"
cat > "${FIX}/remote/dravr-ai/untagged/tags.txt" <<'EOF'
0000000000000000000000000000000000000003	refs/tags/nightly
0000000000000000000000000000000000000004	refs/tags/v0.2
EOF

# A remote whose manifest pins tronc by git tag rather than by version.
cat > "${FIX}/remote/dravr-ai/by-tag/tags.txt" <<'EOF'
0000000000000000000000000000000000000005	refs/tags/v0.3.0
EOF
cat > "${FIX}/remote/dravr-ai/by-tag/v0.3.0/Cargo.toml" <<'EOF'
[package]
name = "dravr-stripe"
version = "0.3.0"

[dependencies]
dravr-tronc = { git = "https://github.com/dravr-ai/dravr-tronc.git", tag = "v1.0.0" }
EOF

# ---------------------------------------------------------------------------
# A consumer repo shaped like the platform: tronc pinned in both manifest
# shapes, embacle-tool-host from crates.io, dravr-stripe by git tag in two
# crates, one of them `optional`.
# ---------------------------------------------------------------------------
make_repo() {
  local dir="$1" stripe_url="${2:-https://github.com/dravr-ai/dravr-stripe.git}"
  mkdir -p "${dir}/crates/core" "${dir}/crates/server"
  cat > "${dir}/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/core", "crates/server"]
EOF
  cat > "${dir}/crates/core/Cargo.toml" <<EOF
[package]
name = "core"
version = "0.1.0"

[dependencies]
dravr-tronc = "0.11.0"
dravr-stripe = { git = "${stripe_url}", tag = "v0.1.13", optional = true }
EOF
  cat > "${dir}/crates/server/Cargo.toml" <<EOF
[package]
name = "server"
version = "0.1.0"

[dependencies]
dravr-tronc = { version = "0.11.0", features = ["notifications"] }
embacle-tool-host = "0.24.0"
dravr-stripe = { git = "${stripe_url}", tag = "v0.1.13" }
EOF
  # The lock before the bump: two tronc entries, which is the very state the
  # step exists to converge.
  cat > "${dir}/Cargo.lock" <<'EOF'
version = 4

[[package]]
name = "dravr-stripe"
version = "0.1.13"
source = "git+https://github.com/dravr-ai/dravr-stripe.git?tag=v0.1.13#25e4cbd682a899bd1abfc1c16f4f1f0cba3c53cb"

[[package]]
name = "dravr-tronc"
version = "0.11.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "dravr-tronc"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "embacle-tool-host"
version = "0.24.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
EOF
}

# The lock a real resolve writes once every pin agrees on tronc 1.0.0.
converged_lock() {
  local dir="$1" stripe_tag="${2:-v0.1.14}" stripe_version="${3:-0.1.14}" stripe_url="${4:-https://github.com/dravr-ai/dravr-stripe.git}"
  cat > "${dir}/Cargo.lock.resolved" <<EOF
version = 4

[[package]]
name = "dravr-stripe"
version = "${stripe_version}"
source = "git+${stripe_url}?tag=${stripe_tag}#afbd8b1b67cd4b48f21b4d9bd2ca16641d0c0fff"

[[package]]
name = "dravr-tronc"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "embacle-tool-host"
version = "0.25.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
EOF
}

# run_step <repo> <version> <lockstep_crates> — runs the lifted step in <repo>
# with the stand-ins first on PATH. Leaves OUT, RC, CALLS and GH_ENV_FILE set.
run_step() {
  local repo="$1" version="$2" lockstep="$3"
  CALLS="${repo}/.calls"; : > "${CALLS}"
  GH_ENV_FILE="${repo}/.github_env"; : > "${GH_ENV_FILE}"
  set +e
  OUT=$(cd "${repo}" && \
        PATH="${STUBS}:${PATH}" FIXTURE="${FIX}" CALLS="${CALLS}" \
        VERSION="${version}" LOCKSTEP_CRATES="${lockstep}" \
        GITHUB_ENV="${GH_ENV_FILE}" RUNNER_TEMP="${WORK}" \
        bash "${STEP}" 2>&1)
  RC=$?
  set -e
}

expect_rc() {
  if [ "${RC}" = "$1" ]; then pass "exit ${RC}"; else fail "expected exit $1, got ${RC}"; printf '%s\n' "${OUT}" | sed 's/^/       | /'; fi
}
expect_out() {
  if printf '%s\n' "${OUT}" | grep -qF -- "$1"; then pass "output has: $1"; else fail "output lacks: $1"; printf '%s\n' "${OUT}" | sed 's/^/       | /'; fi
}
expect_line() {
  # file, exact line
  if grep -qxF -- "$2" "$1"; then pass "${1#${WORK}/} has: $2"; else fail "${1#${WORK}/} lacks: $2"; sed 's/^/       | /' "$1"; fi
}
expect_call() {
  if grep -qxF -- "$1" "${CALLS}"; then pass "called: $1"; else fail "never called: $1"; sed 's/^/       | /' "${CALLS}"; fi
}
expect_call_prefix() {
  # a call whose tail is unpredictable (a mktemp path)
  if grep -qF -- "$1" "${CALLS}"; then pass "called: $1…"; else fail "never called: $1…"; sed 's/^/       | /' "${CALLS}"; fi
}
expect_no_call() {
  # command name, or command + first argument; nothing starting with it may appear
  if grep -q "^$1 " "${CALLS}"; then fail "$1 was called:"; grep "^$1 " "${CALLS}" | sed 's/^/       | /'; else pass "$1 never called"; fi
}

# ---------------------------------------------------------------------------
echo "1. git: entry moves the tag on every pin line to the highest vX.Y.Z tag"
R="${WORK}/s1"; make_repo "${R}"; converged_lock "${R}"
run_step "${R}" 1.0.0 "git:dravr-stripe embacle-tool-host"
expect_rc 0
expect_line "${R}/crates/core/Cargo.toml"   'dravr-stripe = { git = "https://github.com/dravr-ai/dravr-stripe.git", tag = "v0.1.14", optional = true }'
expect_line "${R}/crates/server/Cargo.toml" 'dravr-stripe = { git = "https://github.com/dravr-ai/dravr-stripe.git", tag = "v0.1.14" }'
expect_line "${R}/crates/server/Cargo.toml" 'embacle-tool-host = "0.25.0"'
expect_line "${R}/crates/core/Cargo.toml"   'dravr-tronc = "1.0.0"'
expect_line "${R}/crates/server/Cargo.toml" 'dravr-tronc = { version = "1.0.0", features = ["notifications"] }'
expect_call "git ls-remote --tags https://github.com/dravr-ai/dravr-stripe.git"
expect_call_prefix "git clone --quiet --depth 1 --branch v0.1.14 https://github.com/dravr-ai/dravr-stripe.git ${WORK}/lockstep."
expect_call "cargo update -p dravr-tronc -p dravr-stripe -p embacle-tool-host"
expect_out "lockstep: dravr-stripe -> v0.1.14 (git tag; pins dravr-tronc 1.0.0)"
expect_out "lockstep: embacle-tool-host -> 0.25.0 (requires dravr-tronc ^1.0.0)"
expect_out "Cargo.lock: dravr-tronc 1.0.0 (single entry)"
expect_out "Cargo.lock: dravr-stripe 0.1.14 (single entry)"
expect_out "Cargo.lock: dravr-stripe at v0.1.14"
expect_out "Cargo.lock: embacle-tool-host 0.25.0 (single entry)"
expect_line "${GH_ENV_FILE}" "LOCKSTEP_NOTE=Lockstep: dravr-stripe v0.1.14, embacle-tool-host 0.25.0"
if [ -z "$(find "${WORK}" -maxdepth 1 -name 'lockstep.*' -print -quit)" ]; then pass "the shallow clone is cleaned up"; else fail "a lockstep.* clone was left in RUNNER_TEMP"; fi

echo "2. git: entry whose latest tag pins an older tronc is refused, naming the crate"
R="${WORK}/s2"; make_repo "${R}"; converged_lock "${R}"
run_step "${R}" 0.11.0 "git:dravr-stripe embacle-tool-host"
expect_rc 1
expect_out "::error::dravr-stripe v0.1.14 (latest tag at https://github.com/dravr-ai/dravr-stripe.git) pins dravr-tronc 1.0.0, which cannot resolve to 0.11.0"
expect_out "::error::cut a dravr-stripe tag on dravr-tronc 0.11.0 first, then re-run"
expect_line "${R}/crates/core/Cargo.toml"   'dravr-stripe = { git = "https://github.com/dravr-ai/dravr-stripe.git", tag = "v0.1.13", optional = true }'
expect_line "${R}/crates/server/Cargo.toml" 'dravr-stripe = { git = "https://github.com/dravr-ai/dravr-stripe.git", tag = "v0.1.13" }'
expect_line "${R}/crates/server/Cargo.toml" 'embacle-tool-host = "0.24.0"'
expect_no_call curl
expect_no_call cargo

echo "3. git: entry with no vX.Y.Z tag is refused before any clone"
R="${WORK}/s3"; make_repo "${R}" "https://github.com/dravr-ai/untagged.git"
run_step "${R}" 1.0.0 "git:dravr-stripe"
expect_rc 1
expect_out "::error::lockstep git:dravr-stripe — could not list vN.N.N tags at https://github.com/dravr-ai/untagged.git (auth, or no tags)"
expect_no_call "git clone"
expect_no_call cargo

echo "4. a bare crates.io entry behaves as before and never reaches git"
R="${WORK}/s4"; make_repo "${R}"; converged_lock "${R}" v0.1.13 0.1.13
run_step "${R}" 1.0.0 "embacle-tool-host"
expect_rc 0
expect_line "${R}/crates/server/Cargo.toml" 'embacle-tool-host = "0.25.0"'
expect_line "${R}/crates/server/Cargo.toml" 'dravr-stripe = { git = "https://github.com/dravr-ai/dravr-stripe.git", tag = "v0.1.13" }'
expect_call "curl https://crates.io/api/v1/crates/embacle-tool-host"
expect_call "curl https://crates.io/api/v1/crates/embacle-tool-host/0.25.0/dependencies"
expect_call "cargo update -p dravr-tronc -p embacle-tool-host"
expect_no_call git
expect_line "${GH_ENV_FILE}" "LOCKSTEP_NOTE=Lockstep: embacle-tool-host 0.25.0"

echo "5. a bare crates.io entry on an older tronc is refused with the message it always had"
R="${WORK}/s5"; make_repo "${R}"
run_step "${R}" 0.11.0 "embacle-tool-host git:dravr-stripe"
expect_rc 1
expect_out "::error::embacle-tool-host 0.25.0 (latest on crates.io) requires dravr-tronc ^1.0.0, which cannot resolve to 0.11.0"
expect_line "${R}/crates/server/Cargo.toml" 'embacle-tool-host = "0.24.0"'
expect_no_call git
expect_no_call cargo

echo "6. a git: entry no manifest pins is refused before any remote is asked"
R="${WORK}/s6"; make_repo "${R}"
run_step "${R}" 1.0.0 "git:dravr-photograveur"
expect_rc 1
expect_out "::error::lockstep git:dravr-photograveur — no Cargo.toml here pins dravr-photograveur by git source; remove it from lockstep_crates"
expect_no_call git
expect_no_call cargo

echo "7. a tag whose manifest pins tronc by git tag is read the same way"
R="${WORK}/s7"; make_repo "${R}" "https://github.com/dravr-ai/by-tag.git"; converged_lock "${R}" v0.3.0 0.3.0 "https://github.com/dravr-ai/by-tag.git"
run_step "${R}" 1.0.0 "git:dravr-stripe"
expect_rc 0
expect_out "lockstep: dravr-stripe -> v0.3.0 (git tag; pins dravr-tronc 1.0.0)"
expect_line "${R}/crates/server/Cargo.toml" 'dravr-stripe = { git = "https://github.com/dravr-ai/by-tag.git", tag = "v0.3.0" }'

echo "8. a lockfile that resolves the git crate at the wrong tag fails the assertion"
R="${WORK}/s8"; make_repo "${R}"; converged_lock "${R}" v0.1.13 0.1.14
run_step "${R}" 1.0.0 "git:dravr-stripe"
expect_rc 1
expect_out "::error::Cargo.lock resolves dravr-stripe at tag=v0.1.13, expected v0.1.14"

echo ""
if [ "${FAILURES}" -gt 0 ]; then
  echo "${FAILURES} assertion(s) failed"
  exit 1
fi
echo "all scenarios passed"
