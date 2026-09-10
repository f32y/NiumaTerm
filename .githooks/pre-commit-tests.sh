#!/bin/sh
set -eu

source_root=$(git rev-parse --show-toplevel)
hook="$source_root/.githooks/pre-commit"
scratch=$(mktemp -d)

cleanup() {
    if [ -n "${scratch:-}" ] && [ -d "$scratch" ]; then
        rm -rf -- "$scratch"
    fi
}
trap cleanup EXIT INT TERM

repo="$scratch/repo"
fake_bin="$scratch/bin"
log="$scratch/invocations.log"
mkdir -p "$repo/crates/demo/src" "$repo/vendor/tool/src" "$fake_bin"

printf '%s\n' '[workspace]' 'members = ["crates/demo", "vendor/tool"]' >"$repo/Cargo.toml"
printf '%s\n' '[package]' 'name = "demo"' 'version = "0.1.0"' 'edition = "2024"' >"$repo/crates/demo/Cargo.toml"
printf '%s\n' 'pub fn one() -> u8 { 1 }' >"$repo/crates/demo/src/lib.rs"
printf '%s\n' '#[test]' 'fn one_is_one() { assert_eq!(demo::one(), 1); }' >"$repo/crates/demo/src/tests.rs"
printf '%s\n' '[package]' 'name = "tool"' 'version = "0.1.0"' 'edition = "2024"' >"$repo/vendor/tool/Cargo.toml"
printf '%s\n' 'pub fn value() -> u8 { 1 }' >"$repo/vendor/tool/src/lib.rs"

cat >"$fake_bin/cargo" <<'EOF'
#!/bin/sh
set -eu
if [ "$1" = metadata ]; then
    root=$(pwd -W 2>/dev/null || pwd)
    printf '{"packages":[{"name":"demo","id":"demo-id","manifest_path":"%s/crates/demo/Cargo.toml"},{"name":"tool","id":"tool-id","manifest_path":"%s/vendor/tool/Cargo.toml"}],"workspace_members":["demo-id","tool-id"],"workspace_root":"%s"}\n' "$root" "$root" "$root"
    exit 0
fi
printf 'cargo %s\n' "$*" >>"$HOOK_TEST_LOG"
EOF

cat >"$fake_bin/rustfmt" <<'EOF'
#!/bin/sh
set -eu
printf 'rustfmt %s\n' "$*" >>"$HOOK_TEST_LOG"
EOF

chmod +x "$fake_bin/cargo" "$fake_bin/rustfmt"

cd "$repo"
git init -q
git config user.email test@example.com
git config user.name HookTest
git config core.autocrlf false
git config core.hooksPath .git/no-hooks
git add .
git commit -qm baseline

printf '%s\n' 'pub fn one() -> u8 { 2 }' >"crates/demo/src/lib.rs"
printf '%s\n' '#[test]' 'fn one_is_two() { assert_eq!(demo::one(), 2); }' >"crates/demo/src/tests.rs"
git add .

HOOK_TEST_LOG="$log" PATH="$fake_bin:$PATH" sh "$hook" >"$scratch/success.out" 2>&1

if grep -F 'awk: warning' "$scratch/success.out" >/dev/null; then
    cat "$scratch/success.out" >&2
    exit 1
fi
test "$(grep -c '^rustfmt ' "$log")" -eq 1
grep -F 'crates/demo/src/lib.rs' "$log" >/dev/null
grep -F 'crates/demo/src/tests.rs' "$log" >/dev/null
test "$(grep -c '^cargo clippy ' "$log")" -eq 1
grep -F -- '-- -D clippy::absolute_paths' "$log" >/dev/null

git commit -qm first-party-change
: >"$log"
printf '%s\n' 'pub fn value() -> u8 { 2 }' >"vendor/tool/src/lib.rs"
git add .
HOOK_TEST_LOG="$log" PATH="$fake_bin:$PATH" sh "$hook" >"$scratch/other.out" 2>&1
test "$(grep -c '^cargo clippy ' "$log")" -eq 1
grep -F -- '-p tool' "$log" >/dev/null
if grep -F -- '-D clippy::absolute_paths' "$log" >/dev/null; then
    echo 'pre-commit test: strict lint was applied outside first-party crates' >&2
    exit 1
fi

git commit -qm other-change
: >"$log"
printf '%s\n' '// The task says to keep this branch.' >>"crates/demo/src/lib.rs"
git add .
if HOOK_TEST_LOG="$log" PATH="$fake_bin:$PATH" sh "$hook" >"$scratch/failure.out" 2>&1; then
    echo 'pre-commit test: an instruction-based comment was accepted' >&2
    exit 1
fi
grep -F 'comments must explain technical rationale' "$scratch/failure.out" >/dev/null

# All history manipulation below is confined to the disposable fixture repo.
git reset --hard -q HEAD
git branch -m main
mkdir -p third_party/gpui
printf '%s\n' 'Library update' >third_party/gpui/NOTICE
printf '%s\n' 'pub fn one() -> u8 { 3 }' >crates/demo/src/lib.rs
git add .
if HOOK_TEST_LOG="$log" PATH="$fake_bin:$PATH" sh "$hook" >"$scratch/boundary.out" 2>&1; then
    echo 'pre-commit test: an ordinary mixed vendor commit was accepted' >&2
    exit 1
fi
grep -F 'must be committed separately' "$scratch/boundary.out" >/dev/null
git reset --hard -q HEAD

blocked_word=$(printf '%s%s' 'fa' 'ct')
git checkout -qb upstream
mkdir -p third_party/gpui
printf '%s\n' "$blocked_word" >third_party/gpui/NOTICE
printf '%s\n' 'pub fn one() -> u8 { 3 }' >crates/demo/src/lib.rs
git add .
git commit -qm upstream-change
git checkout -q main
printf '%s\n' 'pub fn one() -> u8 { 4 }' >crates/demo/src/lib.rs
git add .
git commit -qm local-change
if git merge --no-commit upstream >"$scratch/merge.out" 2>&1; then
    echo 'pre-commit test: the fixture did not produce a conflict' >&2
    exit 1
fi
printf '%s\n' 'pub fn one() -> u8 { 5 }' >crates/demo/src/lib.rs
git add .
: >"$log"
HOOK_TEST_LOG="$log" PATH="$fake_bin:$PATH" sh "$hook" >"$scratch/resolved.out" 2>&1
grep -F 'checking resolutions and edits' "$scratch/resolved.out" >/dev/null
grep -F 'crates/demo/src/lib.rs' "$log" >/dev/null
grep -F -- '-- -D clippy::absolute_paths' "$log" >/dev/null

printf '// %s\n' "$blocked_word" >>crates/demo/src/lib.rs
git add .
if HOOK_TEST_LOG="$log" PATH="$fake_bin:$PATH" sh "$hook" >"$scratch/resolution-failure.out" 2>&1; then
    echo 'pre-commit test: a prohibited addition in a resolution was accepted' >&2
    exit 1
fi
grep -F 'refusing to commit AI slop' "$scratch/resolution-failure.out" >/dev/null
printf '%s\n' 'pub fn one() -> u8 { 5 }' >crates/demo/src/lib.rs
git add .
git commit -qm resolved-merge

# A clean merge needs the same scope even when Git leaves no AUTO_MERGE tree.
git checkout -qb clean-upstream
printf '%s\n' "$blocked_word" >inherited.txt
git add .
git commit -qm clean-upstream-change
git checkout -q main
printf '%s\n' 'Local note' >local.txt
git add .
git commit -qm local-note
git merge --no-commit clean-upstream >"$scratch/clean-merge.out" 2>&1
git update-ref -d AUTO_MERGE
HOOK_TEST_LOG="$log" PATH="$fake_bin:$PATH" sh "$hook" >"$scratch/clean.out" 2>&1
printf '%s\n' "$blocked_word" >>local.txt
git add .
if HOOK_TEST_LOG="$log" PATH="$fake_bin:$PATH" sh "$hook" >"$scratch/clean-failure.out" 2>&1; then
    echo 'pre-commit test: an extra edit during a clean merge was accepted' >&2
    exit 1
fi
grep -F 'refusing to commit AI slop' "$scratch/clean-failure.out" >/dev/null

echo 'pre-commit tests passed'
