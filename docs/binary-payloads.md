# Desk binary payloads

File contents use the streaming endpoint only. Upload sends an
`application/octet-stream` body with `X-Desk-Transfer-ID`; download requests
`files.read` with `Accept: application/octet-stream`. File creation, cursors,
commit and cancellation remain small control messages. JSON `files.write`
and JSON file-content responses are no longer supported. File blocks cross
the native interface and desktop worker IPC as bounded raw byte payloads.

LAN browser uploads use XMLHttpRequest to send the original File body. In
the Windows Chromium/Ubuntu lab, the same binary endpoint measured about
33 MiB/s with Fetch and 111 MiB/s with XMLHttpRequest. This changes the
browser upload implementation only: request IDs, CSRF checks, cancellation
and the common response/error parser remain in effect. Connector viewers
continue using the SDK fetch adapter so their uploads stay on the selected
WebRTC or relay transport.

Clipboard HTTP requests and responses use `application/octet-stream` with
this frame, also used across desktop worker IPC and the native interface:

| Offset | Size | Meaning |
| --- | --- | --- |
| 0 | 4 | ASCII `YDC1` |
| 4 | 4 | Little-endian kind: 0 absent, 1 UTF-8 text, 2 RGBA pixels |
| 8 | 4 | Little-endian image width; zero for text/absent |
| 12 | 4 | Little-endian image height; zero for text/absent |
| 16 | variable | Raw UTF-8 or RGBA bytes, at most 16 MiB |

An absent frame has no payload. Empty text is kind 1 with no payload and
remains distinct from absent/unchanged. Image dimensions must be positive,
at most 8192 each, and match exactly `width * height * 4` bytes. Malformed
frames are rejected. An absent frame is the native read request; HTTP
clipboard writes must contain text or an image. Existing authorization,
read-only restrictions, worker generation binding and login-window policy
still apply. There is no JSON/Base64 content fallback.

PNG conversion at the operating-system/browser clipboard boundary remains
necessary for the standard `image/png` clipboard format. Certificate pins,
signed tokens and other authentication formats retain their required Base64
representation; these are separate from file and clipboard payloads.
