#!/bin/sh
set -eu

tag=${1:?release tag is required}
repository=${2:?GitHub repository is required}
dist_dir=${3:-dist}

if ! gh release view "$tag" --repo "$repository" >/dev/null 2>&1; then
    gh release create "$tag" \
        --verify-tag \
        --draft \
        --generate-notes \
        --title "$tag" \
        --repo "$repository"
fi

gh release upload "$tag" "$dist_dir"/* --clobber --repo "$repository"
