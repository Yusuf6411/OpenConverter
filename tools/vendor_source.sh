#!/bin/sh
# Builds openconverter-VERSION.tar.gz with a vendor/ directory for offline builds (COPR, distro
# packagers). Usage: tools/vendor_source.sh VERSION
set -eu
v="${1:?usage: vendor_source.sh VERSION}"
out="openconverter-$v"
rm -rf "/tmp/$out"
mkdir -p "/tmp/$out"
git archive HEAD | tar -x -C "/tmp/$out"
(cd "/tmp/$out" && cargo vendor --locked vendor >/dev/null)
tar -C /tmp -czf "$out.tar.gz" "$out"
echo "wrote $out.tar.gz"
