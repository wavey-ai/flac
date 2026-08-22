#!/bin/sh
set -eu

official_root=${1:-${FLAC_TEST_FILES:-}}
fate_root=${2:-${FATE_SAMPLES:-}}
random_cases=${3:-24}

if [ -z "$official_root" ] || [ -z "$fate_root" ] || [ "$#" -gt 3 ]; then
    echo "usage: $0 FLAC_TEST_FILES FATE_SAMPLES [RANDOM_CASES]" >&2
    echo "       or set FLAC_TEST_FILES and FATE_SAMPLES" >&2
    exit 2
fi

if [ ! -d "$official_root/subset" ] || [ ! -d "$official_root/uncommon" ] || [ ! -d "$official_root/faulty" ]; then
    echo "invalid official FLAC testbench root: $official_root" >&2
    exit 2
fi
if [ ! -d "$fate_root/filter" ] || [ ! -f "$fate_root/cover_art/cover_art.flac" ]; then
    echo "invalid FFmpeg FATE samples root: $fate_root" >&2
    exit 2
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(dirname -- "$script_dir")
cd "$repo_dir"

echo "Running exact and robustness checks over Claxon, official FLAC, and native-FLAC FATE files..."
cargo run --release --example corpus_check -- \
    --required \
    tests/data/claxon/empty_vorbis_comment.flac \
    tests/data/claxon/non_subset.flac \
    tests/data/claxon/pop.flac \
    tests/data/claxon/repeated_vorbis_comment.flac \
    tests/data/claxon/short.flac \
    tests/data/claxon/wasted_bits.flac \
    "$official_root/subset" \
    "$fate_root/filter" \
    --optional \
    "$official_root/uncommon" \
    --robustness \
    tests/data/claxon/large_vendor_string.flac \
    tests/data/claxon/large_vorbis_comment_block.flac \
    tests/data/claxon/fuzz \
    "$official_root/faulty" \
    "$fate_root/cover_art/cover_art.flac"

echo "Running deterministic randomized Wavey/FFmpeg/libFLAC cross-encoding checks..."
cargo run --release --example randomized_differential -- "$random_cases"
