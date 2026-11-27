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
- The original acceptance ran on Wayland. Portal capture worked; DRM capture failed
  in the original accelerated VMware setup; the later no-3D result is below.
  The later X11 regression below ran after the user changed the desktop session.
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


## X11 regression after the desktop session change

Confirmed the active seat and Desk desktop worker both use X11. Restarted
only the native acceptance fixture after logout; the deployed binaries were
unchanged. The final service remained active and Connector remained logged in.

Both LAN and host-to-host UDP STUN passed:

- Native fixture pixel checks, changing video frames, keyboard text and mouse.
- Text and image clipboard in both directions with native RGBA pixel checks,
  no echo, viewer read-only pause and server read-only rejection.
- Viewer mode switches, automatic connection/reconnection, network recovery,
  read-only input rejection and three forced disconnect/reconnect rounds.
- File navigation, empty and Unicode names, duplicate names, 64 small files,
  and 128 MiB upload/download with SHA-256 integrity checks.
- Bidirectional chat, history after reconnect, no duplicates, HTML rendered as
  text, read-only rejection and local-admin Host/Origin/public-route checks.

The file sample measured about 104/95 MiB/s LAN and 53/30 MiB/s STUN
upload/download including UI completion. STUN download remains below the old
40 MiB/s target; functional integrity passed and physical-link tuning was not
resumed. Legacy file fault-injection hooks remain outside this basic suite.
Chat testing used a separate SSH forwarding port with the original admin
Host/Origin preserved; a changed port correctly triggered Host validation.
Reports are saved locally with the `retest-ubuntu-x11-` prefix.


## Wayland DRM regression with VMware 3D acceleration disabled

After disabling VMware 3D acceleration, the user initially saw a black desktop.
The GNOME user session was still running, using vmwgfx atomic KMS. Backed up
and removed the stale monitor configuration and installed this user-scoped
GNOME Shell Wayland service drop-in:

```ini
# ~/.config/systemd/user/org.gnome.Shell@wayland.service.d/60-vmware-no3d-kms.conf
[Service]
Environment=MUTTER_DEBUG_FORCE_KMS_MODE=simple
```

After restarting GDM and the user logging in, GNOME logged non-atomic mode
setting. This retains Wayland and DRM; it does not enable VMware 3D acceleration.
Monitor/configuration backups are under
`~/yoodesk-acceptance-20261127/wayland-no3d-recovery`. To undo the KMS override,
remove that drop-in, reload the user systemd manager, then log in again.

Enabled only `enable_drm` in the existing service configuration, preserving
identity/account settings. The preceding config is backed up at
`/var/backups/yoodesk-acceptance-20261127/config-before-drm-no3d.json`.
Verified a root DRM helper, an active Wayland desktop worker with
`YOODESK_ENGINE_DRM_ENABLED=1`, and online Connector before/after the tests.
The DRM profile disables the native Portal capture branch.

Both LAN and host-to-host UDP STUN passed native changing-pixel video checks,
keyboard/mouse input, text and image clipboard with native RGBA checks,
read-only enforcement, viewer mode switches, network recovery, three forced
reconnects, basic files and bidirectional chat/history. The file suite included
64 small files and 128 MiB uploads/downloads with SHA-256 integrity checks.
Observed UI-completion speeds were about 103/96 MiB/s LAN and 48/35 MiB/s
STUN upload/download; the physical-link limit remains accepted, rather than
claiming the old 40 MiB/s download gate passed.

System audio also passed on both paths: the Ubuntu host played a native test
tone and the browser decoded real audio with nonzero energy. This checks audio
capture/transport/decoding, not a recording/export or a listening-quality test.
Reports use the local `retest-ubuntu-drm-` prefix. The GTK/audio fixtures were
stopped after acceptance; the service is left online in Wayland DRM mode.
