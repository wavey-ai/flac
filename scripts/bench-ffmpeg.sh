#!/bin/sh
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 3 ]; then
    echo "usage: $0 INPUT-24-BIT.wav [RUNS] [OUTPUT_DIR]" >&2
    exit 2
fi

input=$1
case $input in
    /*) ;;
    *) input=$(pwd)/$input ;;
esac
runs=${2:-5}
output_dir=${3:-target/ffmpeg-baseline}
script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(dirname -- "$script_dir")

cd "$repo_dir"
exec cargo run --release --example ffmpeg_baseline -- "$input" "$output_dir" "$runs"
