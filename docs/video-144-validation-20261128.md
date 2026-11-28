# Hardware capture and 144 FPS validation

Windows LAN acceptance used the exact artifact from
[run 46051372948](https://github.com/YooDesk/desk/actions/runs/46051372948),
source `fb2381afa5a646937a1d5934149742754885ae54`, DLL SHA256
`968d37bead619ca490d5b3849c1b8ac70d11536d6679820704f445b0ea1814a9`.

On the local RTX 5060 Ti machine, the physical display remained at 2560 × 1440,
144 Hz. A visible full-DPI animation was captured through native LAN HTTPS and
decoded by headless Edge WebCodecs to avoid recursive capture. Over 30 seconds
after warm-up, source FPS was 144.0002, captured/encoded FPS was 144.012 and
decoded FPS was 144.0016. GPU texture capture and NVENC remained active. There
were zero decoder errors or reconfigurations, including a deliberate 40 ms
viewer JavaScript pause. This measures decoded throughput; headless decoding
does not establish the presentation rate of a visible browser window.

Separate hardware and forced-software static-image tests verified the first
image and final update against known source pixels. Those tests establish image
correctness and fallback, not software encoding throughput at 144 FPS.

Local evidence is under `artifacts/hardware-pipeline/`:

- `lan-ci-46051372948-1795805791/report.json`: motion throughput.
- `lan-ci-46051372948-1795806090/report.json`: hardware static-image checks.
- `lan-ci-46051372948-1795806109/report.json`: software static-image checks.

## GitHub component builds

All runs use build-only mode and do not publish releases.

| Target | Run | Source | Result |
| --- | --- | --- | --- |
| Windows x64 | [46051372948](https://github.com/YooDesk/desk/actions/runs/46051372948) | `fb2381a` | Passed; local LAN acceptance above |
| Windows x86 | [46059404585](https://github.com/YooDesk/desk/actions/runs/46059404585) | `58dd35e` | Passed |
| Windows ARM64 | [46059407842](https://github.com/YooDesk/desk/actions/runs/46059407842) | `58dd35e` | Passed |
| Linux x64 | [46054146764](https://github.com/YooDesk/desk/actions/runs/46054146764) | `fb2381a` | Passed |
| Linux ARM64 | [46059414121](https://github.com/YooDesk/desk/actions/runs/46059414121) | `58dd35e` | Passed |
| macOS ARM64 | [46055359068](https://github.com/YooDesk/desk/actions/runs/46055359068) | `86538f4` | Passed |
| macOS x64 | [46059410883](https://github.com/YooDesk/desk/actions/runs/46059410883) | `58dd35e` | Passed |
| Android ARM64 component | [46054155358](https://github.com/YooDesk/desk/actions/runs/46054155358) | `fb2381a` | Passed |

The changes after the Windows-tested source enable the missing macOS x264
dependency and guard SDK publication in build-only mode. Windows runtime code
is unchanged by those changes. Dependencies use the pinned RustDesk/vcpkg
recipes with reviewed adapters in isolated build worktrees. Port and dependency
ABI changes invalidate the relevant binary cache entries.

Windows x86 additionally passed 11 SDK video tests, the actual FFmpeg input-pool
ownership test, six bitmap tests and 110 main-library tests (seven ignored).
Its exact artifact was checked for source and content hashes, x86 PE architecture,
system DLL dependencies and absence of dynamically linked CRTs.

Windows ARM64 passed 11 SDK video tests, six bitmap tests and 110 main-library
tests (seven ignored). Its exact DLL passed ARM64 PE architecture, source/content
hashes, system DLL dependency and static CRT checks. The archive check on this
architecture verifies x264 software fallback; it does not establish availability
of a hardware encoder on a Windows ARM64 device.

Linux ARM64 passed 11 SDK video tests and 110 main-library tests (eight ignored).
Both shipped ELF files were independently inspected: the plugin requires at most
glibc 2.27 and libdrmtap at most glibc 2.17; both are AArch64.

macOS x64 passed 11 SDK video tests, 102 main-library tests (four ignored) and
14 Python checks. Its exact artifact passed hash, x86_64 Mach-O, system-framework
dependency and build-rpath checks. The FFmpeg archive contains both x264 and
VideoToolbox H.264 encoders.

The Android CI artifact exports all four encoded v1 bridge functions. Current
client JNI glue successfully cross-links against that exact artifact with NDK
26.1.10909125, API 24, warnings as errors, no undefined strong symbols and
16 KiB load alignment. A separate local APK packaging/signature/payload check
uses the six pinned released components. The older released Desk component
keeps software fallback until a matching new component is released. No release
pins were replaced with unpublished artifacts.

## Formal installation and remote Gateway acceptance

The local installed service was upgraded while preserving its existing identity,
password, certificates and configuration, with FPS set to 144. Its Host and Desk
hashes match the accepted Windows binaries above. LAN access through the installed
service decoded 143.8469 FPS over 30 seconds, with a 144.0107 FPS animation source.

The installed Connector initially differed from the current published component.
After upgrading it to `v0.1.0-ci.70` (`cdc167a`) and restarting the service, the
existing account remained logged in, the Gateway connection recovered and the
account/device identity fingerprint was unchanged. The installed Connector SHA256
is `fec9dd65512f20cc7915ffa6f0c930104582689e240c643ffd3df5be6a680eea`.

Using that existing device authorization and the normal invitation flow, the
default resumable transport negotiated through the real remote Gateway and
remained on STUN for the entire measurement. Over 30 seconds after warm-up,
actual decoded FPS was 143.5685 and source animation FPS was 143.9850. The test
included a deliberate 40 ms viewer JavaScript pause. Decoder errors, chunk errors
and decoder reconfigurations were zero; decoded fixture pixels changed throughout.
This meets the 144 FPS target within measurement/scheduling variation. It is a
same-machine direct-ICE test through remote Gateway negotiation, not evidence of
144 FPS across two physical networks or visible-browser presentation throughput.

The deployed Gateway is the previously CI-verified `v0.1.0-ci.60` at `c90a011`.
The running binary SHA256 matches
`88167ca50a87e25b55e5aea4d863fee4a89dacc01f39ef9bc56cb1a48dc9e641`.
The previous container and original data directory were retained for rollback;
the new container uses a cold copy of the existing data and unchanged configuration
and certificates. Public HTTPS, STUN binding and TURN TCP reachability passed.

Local evidence:

- `../gateway/artifacts/remote-144/lan-installed-1795826054190/report.json`.
- `../gateway/artifacts/remote-144/stun-installed-1795827623306/report.json`.
- `../connector/artifacts/ci70-windows-x64/post-upgrade-auth-proof.json`.
- `../gateway/artifacts/deploy-c90a011/deploy-proof.json`.

## Outstanding acceptance

Other-platform physical throughput remains for the user's machines. These build
results do not establish their actual capture, encode or decode rate.

The user authorized temporarily making one private repository public at a time.
This removed the billing startup restriction: WebService desktop run
46086813168 passed all ten architectures, and APK run 46087638757 passed its six
Android tests, including native JNI compatibility, plus packaging and payload
checks. Both repositories were restored to private. Android Host build/release
46091214583 also passed, producing `android-v0.1.0-ci.7` at `79b49bd` with an
embedded 144 FPS request, then restoring repository privacy.

The complete Desk native matrix at `9bee5ab` (run 46087732528) passed all ten
architectures. Verified artifacts were published as `v0.1.0-ci.182` by successful
promotion run 46092325771. Android publication run 46091206701 also passed,
producing `android-v0.1.0-ci.7`. Matching WebService desktop release run
46092534207 is assembling the newly released plugins before final installer
packages are built. The earlier APK compatibility build used
an older released Desk component and is not the final hardware-enabled package.

Password re-login previously returned HTTP 401, but that does not block the
already authorized local Connector. Its existing account/device authorization
was verified and used successfully for the actual remote Gateway/STUN test above.
