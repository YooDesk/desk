# Ubuntu acceptance, 2026-11-27

The final user instruction accepts physical-link and virtual-NIC losses and
limits further optimization to protocol problems. The earlier 50 MiB/s and
link-saturation targets no longer act as hard performance gates.

## Changes deployed

- File contents cross HTTP, worker IPC and native interfaces as binary bytes.
  Removed the browser and engine JSON/Base64 file-content fallbacks.
- Text and image clipboard contents use the bounded binary frame documented
  in [binary-payloads.md](binary-payloads.md), including the Wayland Portal path.
- LAN uploads send the original File through XMLHttpRequest. Connector
  viewers retain the SDK fetch adapter and its selected transport.
- Connector uses a bounded pool of two dedicated WebRTC reactors.
- Gateway retains the authorized, non-secret device selector on top-level
  Desk/VNC/SSH/RDP pages. Refresh obtains fresh page authority rather than
  reusing a page session released during navigation.
- Removed two unused WebService clipboard/file script copies. Authentication
  and certificate formats retain their required Base64 encoding.

Ubuntu runs the locally built Desk plugin and the published ci.44 host.
The Gateway was built locally and replaced inside the existing container.
Identity and account configuration were preserved.

## Verified results

| Area | Result |
| --- | --- |
| Desk LAN and STUN file flow | Navigation, empty/Unicode names, duplicate names, 64 small files, 128 MiB upload/download with native SHA-256 checks passed |
| Desk LAN and STUN clipboard | Text and image synchronization in both directions, native PNG/RGBA pixel checks, no echo, viewer pause and server read-only-session denial passed |
| Desk LAN speed after final change | About 111 MiB/s upload payload, 104 MiB/s including UI completion, 88 MiB/s download |
| Desk STUN speed | Latest run about 48 MiB/s upload payload and 32 MiB/s download; host-to-host UDP selected, Gateway HTTP business fallback blocked during the test |
| SSH LAN | Earlier formal streaming run about 114/123 MiB/s upload/download |
| SSH STUN | Latest sampled run about 46/35 MiB/s upload/download; bidirectional hashes passed |
| VNC STUN refresh | Three consecutive refreshes recovered the connection and fresh, changing native fixture pixels; access/VNC credentials were supplied again when prompted |
| Rust tests | Windows Desk library 108 passed, 7 ignored; Linux Desk library 108 passed, 8 ignored; native clipboard 7 passed |
| Browser tests after final LAN change | 47 passed, 1 existing skip |

Time-aligned Connector profiling places the largest download CPU cost in
the guest e1000 transmit driver. This is recorded as an environment limit;
the results do not establish that every possible transport bottleneck has
been eliminated. No further physical-link tuning is required by the user.

## Limits and outstanding findings

- The basic file suite omits legacy JSON fault-injection hooks. It is not a
  claim that the original complete fault-injection suite passed.
- The desktop seat is Wayland. Portal capture works; DRM capture failed.
- Wayland keyboard input is fixed. The product-specific virtual keyboard now
  receives active-seat access through a scoped udev/logind rule; physical input
  devices receive no additional access. The pinned evdev adapter waits up to
  two seconds for the new event node and its ACL before opening it.
- Full Desk LAN and STUN regressions passed on the actual Wayland seat with
  Portal capture: native keyboard text, mouse, live video, bidirectional text
  clipboard, read-only input denial, mode switching, network recovery and
  three forced reconnects per transport. STUN selected host-to-host UDP.
- The new Debian installer includes the scoped keyboard rule and reloads
  udev before starting the service. The acceptance VM received the same rule
  alongside the locally rebuilt plugin; its published ci.44 host was retained.
- VNC initial LAN/STUN video and input passed on an isolated Xvfb fixture,
  not the Wayland seat. LAN refresh passed; STUN refresh now also passed
  after the Gateway page-authority fix above.
- No RDP acceptance was requested in this Ubuntu run.

Backups remain under `/var/backups/yoodesk-acceptance-20261127`. Detailed
machine-readable test reports are in the local ignored
`yoodeskclient/artifacts/deployed-local` directory. Experimental HTTP packing,
window growth, large send buffers and upload read-ahead that showed no
benefit were reverted.
