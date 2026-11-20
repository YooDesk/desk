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
