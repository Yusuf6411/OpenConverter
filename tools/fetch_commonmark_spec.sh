#!/bin/sh
# Download the CommonMark and GFM specifications (CC BY-SA 4.0) for the Markdown conformance
# test. They are not stored in this repository.
set -e
mkdir -p target/spec
curl -sSL -o target/spec/commonmark.txt https://raw.githubusercontent.com/commonmark/commonmark-spec/master/spec.txt
curl -sSL -o target/spec/gfm.txt https://raw.githubusercontent.com/github/cmark-gfm/master/test/spec.txt
