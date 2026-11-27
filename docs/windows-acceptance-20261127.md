# Windows installer and Desk/RDP acceptance, 2026-11-27

## Installed package

Locally rebuilt the Windows x64 Connector, WebService and all four plugins
from the current split repositories, including a fresh Desk native engine.
This is a source build, not a previously published CI installer. The immutable
component/archive locks and full source/native provenance are retained locally
in `yoodeskclient/artifacts/local-windows`.

- Installer: `yoodeskclient-0.1.0+local.20261127-windows-x64.exe`
- SHA-256: `866d5823861c700e8f7737e4e33de4da3f65d12f3c2c831354213e4f2aec8d90`
- The actual NSIS payload was extracted and all 61 manifest files verified;
  Windows dependency/CRT checks passed during component packaging.
- Canonical runtime: `C:/Program Files/yoodesk`, service `yoodesk`.
- Canonical data: `C:/ProgramData/yoodesk`.

Migrated the active legacy configuration into the canonical data directory,
updating old absolute paths and the explicit Global Gateway configuration.
Device identity, TLS certificate/private key, access key and Connector backend
key were preserved and checked. Installed host/Connector binaries and all
plugin hashes match the verified local package. The new service and actual VM Connector are
online. Existing controller-local admin port 19446 must not be mistaken for
the VM: acceptance used a separate SSH forward with the original admin
Host/Origin preserved.

The old unused canonical program directory had inherited legacy ACLs that
failed the new installer's protected-directory checks. Moved it to a secured
backup, then installed at the canonical path. Removed both old service names,
the `yoodesk-WebService` and `YooDeskClient` program directories, obsolete
startup tasks, legacy uninstall registration and the two exact old HTTPS
firewall rules. Four current application firewall rules remain. Original
configuration/program backups remain under
`C:/ProgramData/YooDeskLab/upgrade-20261127`; no account or credential data was
discarded.

## Functional results

All remote STUN checks selected host-to-host UDP and blocked Gateway HTTP
business fallback. Tests ran serially against the real Windows desktop and
native input/file/clipboard witnesses.

| Check | LAN | STUN |
| --- | --- | --- |
| Desk changing native pixels and keyboard/mouse | Passed | Passed |
| Desk text clipboard both directions | Passed | Passed |
| Desk read-only input denial, held-key release and viewer mode switches | Passed | Passed |
| Desk network recovery and three forced reconnects | Passed | Passed |
| Desk basic file navigation, Unicode/empty/duplicate names, 64 small files and 128 MiB bidirectional SHA-256 | Passed | Passed |
| Desk bidirectional chat and history/read-only protections | Passed | Passed |
| Desk browser-to-native image RGBA | Passed | Passed |
| Desk native-to-browser image RGBA | Fixed; clipfix passed | Fixed; clipfix passed |
| Desk clipboard no-echo, viewer read-only pause and server read-only denial | Passed | Passed |
| RDP changing native pixels, keyboard/mouse and text clipboard | Passed | Passed |
| RDP five forced reconnects, fresh pixels and input after reconnect | Passed | Passed |
| RDP real Explorer copy to browser and browser file paste to Explorer, 2,097,169 bytes each with native SHA-256 | Passed | Passed |

Desk file samples including UI completion were about 99/80 MiB/s LAN and
19/29 MiB/s STUN upload/download. The latter does not pass a 40 MiB/s speed
gate. Integrity passed; no throughput stress or new physical-link tuning was
performed, and this run does not assign a proven cause to Windows throughput.
Audio, recording/export, privacy screen and cross-network NAT traversal are
outside this functional suite.

## Resolved image-clipboard failure

The native fixture writes an opaque 2x2 Windows bitmap. Both its own pixel
witness and a separate interactive process read the expected native RGBA.
Windows synthesizes CF_DIBV5 with a 124-byte header, 16-byte `sizeImage`,
BI_BITFIELDS compression and an additional 12 bytes of RGB masks before the
actual pixel data (152 bytes total). The pinned arboard reader starts pixels
after the 124-byte header and interprets those masks as pixel data. Browser
pixels reproduce the resulting mask colors and shifted source pixel.

This establishes a Windows bitmap-reader compatibility failure, rather than
binary-wire corruption or STUN packet loss. The follow-up fix below resolves this
reader failure and the installed candidate now passes both paths. The raw
sample and independent witness are local test artifacts containing only the
known test pixels. Other completed results above remain valid.


## Installed clipboard fix

Rebuilt and installed `yoodeskclient-0.1.0+clipfix.20261127-windows-x64.exe`,
SHA256 `8a10ed5e26ac52d2184d9aae58969b6e13fd6f844e330fb6bf4d3d3dcd521bd7`.
The actual installer again passed all 61 payload-file checks. The native Desk
engine was rebuilt; configuration, identity, certificates and access/backend
keys were preserved, plugin hashes checked and the VM account stayed online.
Backup: `C:/ProgramData/YooDeskLab/upgrade-20261127/before-clipfix`.

For opaque DIBV5 clipboard images, the pinned arboard adapter now reads the
Windows-owned CF_BITMAP through GDI rather than guessing a pixel offset in a
synthesized DIB. It leaves the borrowed bitmap handle owned by the clipboard,
releases the temporary screen DC, bounds output to 16 MiB, validates row
completion and emits the same raw RGBA binary frame. Explicit-alpha DIBV5
retains the existing reader and transparency behavior. No PNG/Base64 payload
fallback was added. The adapter is materialized from the exact pinned Git
commit into ignored artifacts, without editing Cargo's shared source cache.

All six Windows GDI tests passed: row order/colors at multiple widths, repeated
reads of the borrowed bitmap, invalid/oversized dimensions, explicit alpha,
existing missing-alpha handling and color conversion. Windows native CI now
runs these tests via `scripts/test_windows_bitmap.py` using the pinned crate's
own immutable test lock.

After the real package upgrade, both LAN and host-to-host STUN passed text
and image clipboard in both directions, native RGBA pixel checks, no echo,
viewer read-only pause and server-side read-only rejection. Local reports use
`windows-clipfix-` prefixes. RDP's plugin and viewer/SDK assets are byte-identical
to the already accepted build. The test fixture was stopped afterward; the
canonical service remains online. GitHub CI results are separate from the
local Windows tests and deployed acceptance reported here.
