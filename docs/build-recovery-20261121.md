# Pinned GitLab archive URL recovery

The original run `44767568435` compiles source `fa3b03c20e32012d92792c4a0a022b120776acfd`.
Windows x64, Windows ARM64 and Linux ARM64 could not download the pinned x264 archive:
vcpkg joined a trailing-slash GitLab base URL to `/videolan/x264`, producing `//videolan`.
The server closes this malformed request without a response (curl 52/56).

`scripts/prepare_vcpkg_downloads.py` normalizes only trailing slashes in the pinned vcpkg
GitLab helper. The x264 commit and SHA512 stay unchanged. Local validation executed the
actual pinned CMake helper, checked idempotence, downloaded the canonical URL, and matched
the exact upstream SHA512. The workflow also passed actionlint.

To preserve successful platform builds, the desktop workflow accepts `source_ref` and
`reuse_run_id`. Reuse is permitted only when the completed run identifies that exact source
commit and the workflow commit differs only in the reviewed build scripts or documentation.
The target matrix omits existing non-expired native artifacts; the release job combines
those artifacts with the newly completed platforms and verifies all eight source commits,
platform names, archive checksums and DLL/library hashes.

Native builds check out `source_ref`; only the build/download adapters come from the reviewed
workflow commit. Every package records the actual pinned source SHA, and the release tag
targets that same SHA. Runtime code is never substituted by the artifact reuse path.

The original Linux x64 run also encountered HTTP 504/connection timeouts from the
existing Canonical Azure Bionic mirror during the later desktop dependency install.
All APT phases now use three bounded acquisition retries and 30-second HTTP/HTTPS
timeouts. The Ubuntu 18.04 environment, package versions and mirror remain unchanged;
an exhausted download still fails the build.

## Release-only recovery

All eight native archives passed validation. The release integration token rejected creating/updating a release with an older target_commitish (403). The tag was explicitly created at the exact runtime source. publish-recovery.yml downloads the two completed runs, checks the tag, all eight platforms, archive SHA256 and embedded source/library hashes, then uploads to that existing tag without changing target_commitish. No platform is recompiled.
