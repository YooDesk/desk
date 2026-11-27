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
| Desk native-to-browser image RGBA | Failed | Failed |
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

## Confirmed image-clipboard failure

The native fixture writes an opaque 2x2 Windows bitmap. Both its own pixel
witness and a separate interactive process read the expected native RGBA.
Windows synthesizes CF_DIBV5 with a 124-byte header, 16-byte `sizeImage`,
BI_BITFIELDS compression and an additional 12 bytes of RGB masks before the
actual pixel data (152 bytes total). The pinned arboard reader starts pixels
after the 124-byte header and interprets those masks as pixel data. Browser
pixels reproduce the resulting mask colors and shifted source pixel.

This establishes a Windows bitmap-reader compatibility failure, rather than
binary-wire corruption or STUN packet loss. No product fix is included in this
acceptance run; image clipboard cannot be reported as fully passed. The raw
sample and independent witness are local test artifacts containing only the
known test pixels. Other completed results above remain valid.
