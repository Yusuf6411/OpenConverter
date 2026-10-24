#!/bin/bash -eu
cd $SRC/openconverter/fuzz
cargo fuzz build -O --debug-assertions
for t in fuzz_targets/*.rs; do
  n=$(basename "$t" .rs)
  cp "target/x86_64-unknown-linux-gnu/release/$n" "$OUT/$n"
done
