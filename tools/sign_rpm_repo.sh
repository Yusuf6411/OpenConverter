#!/bin/sh
# Builds and signs a DNF/YUM repository from RPMs. Needs createrepo_c and gpg.
# Usage: tools/sign_rpm_repo.sh RPM_DIR GPG_KEY_ID
set -eu
dir="${1:?usage: sign_rpm_repo.sh RPM_DIR GPG_KEY_ID}"
key="${2:?gpg key id}"
for f in "$dir"/*.rpm; do rpm --addsign "$f" --define "_gpg_name $key" 2>/dev/null || true; done
createrepo_c "$dir"
gpg --batch --yes --local-user "$key" --detach-sign --armor "$dir/repodata/repomd.xml"
gpg --export --armor "$key" > "$dir/RPM-GPG-KEY-openconverter"
echo "signed repo in $dir"
