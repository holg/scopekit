#!/usr/bin/env bash
# File: scripts/test-ci-locally.sh
# Local check script for scopekit.
#
# Runs what .github/workflows/ci.yml runs (fmt, HOWTO copy, clippy over
# every feature combination, tests), plus the checks CI does not run yet:
# rustdoc warnings, TOML formatting, typos, cargo-deny and a package
# dry run. Optional tools are skipped with a hint when not installed.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

AUTO_FIX=false
SKIP_TESTS=false

print_usage() {
    cat << EOF
Usage: $0 [OPTIONS]

Options:
    --fix           Auto-fix what can be fixed (cargo fmt, taplo, HOWTO.md copy)
    --no-test       Skip cargo test (GPU tests are the slow part)
    -h, --help      Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
    case $1 in
        --fix) AUTO_FIX=true; shift ;;
        --no-test) SKIP_TESTS=true; shift ;;
        -h|--help) print_usage; exit 0 ;;
        *)
            echo -e "${RED}Unknown option: $1${NC}"
            print_usage
            exit 1
            ;;
    esac
done

cd "$(dirname "$0")/.."

step() { echo -e "${YELLOW}$1${NC}"; }
pass() { echo -e "${GREEN}✓ $1${NC}\n"; }
fail() { echo -e "${RED}✗ $1${NC}"; [[ -n "${2:-}" ]] && echo -e "${YELLOW}$2${NC}"; exit 1; }
missing() { echo -e "${YELLOW}⚠ $1 not installed, skipping${NC}"; echo -e "${YELLOW}Install with: $2${NC}\n"; }

echo -e "${BLUE}=== scopekit local CI ===${NC}\n"

# Step 1: rustfmt
step "Step 1: cargo fmt check..."
if cargo fmt --all -- --check; then
    pass "cargo fmt passed"
elif [[ "$AUTO_FIX" == true ]]; then
    cargo fmt --all
    pass "cargo fmt auto-fixed"
else
    fail "cargo fmt failed" "Run 'cargo fmt --all' or use --fix"
fi

# Step 2: the root HOWTO.md is the crate's copy (the crate ships its own).
step "Step 2: HOWTO.md matches crates/scopekit/HOWTO.md..."
if cmp -s HOWTO.md crates/scopekit/HOWTO.md; then
    pass "HOWTO.md copies match"
elif [[ "$AUTO_FIX" == true ]]; then
    # The root copy is the one edited; the crate copy follows it.
    cp HOWTO.md crates/scopekit/HOWTO.md
    pass "copied HOWTO.md to crates/scopekit/HOWTO.md"
else
    fail "HOWTO.md and crates/scopekit/HOWTO.md differ" \
         "Run 'cp HOWTO.md crates/scopekit/HOWTO.md' or use --fix"
fi

# Step 3: clippy over every feature combination, as in CI.
step "Step 3: clippy, every feature combination..."
clippy_runs=(
    "--workspace --all-targets --all-features"
    "-p scopekit --no-default-features --features terminal"
    "-p scopekit --no-default-features --features window"
    "-p scopekit --no-default-features"
)
for args in "${clippy_runs[@]}"; do
    echo "cargo clippy $args"
    # shellcheck disable=SC2086
    cargo clippy --locked $args -- -D warnings || fail "clippy failed: $args"
done
pass "clippy passed"

# Step 4: rustdoc, broken links and the like are errors.
step "Step 4: cargo doc..."
if RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps; then
    pass "cargo doc passed"
else
    fail "cargo doc failed"
fi

# Step 5: TOML formatting (taplo.toml).
step "Step 5: taplo format check..."
if command -v taplo &> /dev/null; then
    if taplo format --check; then
        pass "taplo passed"
    elif [[ "$AUTO_FIX" == true ]]; then
        taplo format
        pass "taplo auto-fixed"
    else
        fail "taplo failed" "Run 'taplo format' or use --fix"
    fi
else
    missing taplo "cargo install taplo-cli"
fi

# Step 6: spelling (_typos.toml).
step "Step 6: typos..."
if command -v typos &> /dev/null; then
    if typos; then
        pass "typos passed"
    else
        fail "typos failed" "Fix the words, or whitelist them in _typos.toml"
    fi
else
    missing typos "cargo install typos-cli"
fi

# Step 7: advisories, bans, licenses, sources (deny.toml).
step "Step 7: cargo deny check..."
if command -v cargo-deny &> /dev/null; then
    if cargo deny check --hide-inclusion-graph; then
        pass "cargo deny passed"
    else
        fail "cargo deny failed"
    fi
else
    missing cargo-deny "cargo install cargo-deny"
fi

# Step 8: tests. GPU tests skip themselves where there is no adapter.
if [[ "$SKIP_TESTS" == true ]]; then
    echo -e "${YELLOW}Step 8: tests skipped (--no-test)${NC}\n"
else
    step "Step 8: cargo test..."
    if cargo test --locked --workspace --all-features; then
        pass "tests passed"
    else
        fail "tests failed"
    fi
fi

# Step 9: the crate packages and builds from its tarball alone.
step "Step 9: cargo publish --dry-run..."
if cargo publish --locked --dry-run --allow-dirty -p scopekit; then
    pass "publish dry run passed"
else
    fail "publish dry run failed"
fi

echo -e "${GREEN}=== All checks passed! ===${NC}"
