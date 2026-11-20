# Viewer carrier resume (draft)

The resume stream belongs to a single authenticated viewer session and lives
below yamux. Both endpoints must explicitly negotiate framing before using it.

Carrier activation is separate from the authorization generation. After the
transport has been authenticated for the logical session, the browser sends
the four ASCII bytes `YCR1` followed by an eight-byte big-endian nonzero
carrier generation (at most 2^53 - 1). The Connector validates this against
the generation approved for that candidate, then echoes the exact 12 bytes.
The browser waits for the echo before switching. The next byte in either
direction starts normal record framing. An unexpected preface, timeout, or
stale generation rejects the candidate. The preface does not authenticate a
holder or grant access; it must not be accepted from an unauthenticated path.

On an authorized WSS relay stream, the first byte distinguishes the explicit
YCR1 bootstrap from a legacy yamux version-zero stream. The first WSS carrier
uses generation 1 and still requires the inner yamux grant bind. Connectors
without resume support do not accept the YCR1 preface.
Subsequent WSS carriers require a newly authorized outer relay stream for the
same session and authorization generation, and a strictly newer carrier
generation. The Connector keeps the logical stream until its lease ends, the
inner session closes, or the owning device data tunnel closes.

An authorized WebRTC peer may open the reliable ordered DataChannel
`raccess.resume.v1` to attach to an existing resumed session with the same
authorization generation. It uses the same preface and record framing. This
label cannot create a new logical session or replace the inner grant bind.
One candidate WebRTC peer may be prepared while one previous peer continues
carrying data. The previous peer is released after its carrier detaches; an
unsuccessful candidate must not detach it.

Each record begins with a type byte, an unsigned 64-bit big-endian offset
capped at 2^53 - 1, and a 16-bit big-endian payload length. Records can be
split or coalesced by the transport. Type 0 carries 1 to 16373 bytes beginning at the
offset. Type 1 has no payload and acknowledges receipt of all bytes before the
offset in the opposite direction. Each direction starts at offset zero.

Use reliable ordered carriers. The sender retains at most 256 KiB of
unacknowledged bytes per direction and retransmits them in order on carrier
replacement. The receiver discards duplicates, rejects gaps, and acknowledges
only delivered bytes. Old-carrier callbacks cannot affect the resumed stream.
Exhausting the window applies backpressure; closing the logical stream discards
all state. Never resume across a viewer session boundary.

The viewer starts on WSS and first probes direct WebRTC without TURN. On a
connectivity failure it starts a fresh ICE round with direct and authorized
TURN candidates under ICE's normal direct-candidate priority. If TURN is not
authorized or available, the second round is direct-only. WSS continues to
carry business traffic until the new carrier is confirmed. On carrier
loss, an in-progress candidate may still attach. If none attaches, the
viewer makes bounded attempts on WSS, direct WebRTC, and authorized TURN.
If no replacement is available, it closes the logical stream instead of
waiting indefinitely. Revocation or session replacement ends recovery.

Direct candidates use the current lease deadline. An expired TURN
configuration must not be sent to the Connector. If the preparer cannot
refresh it without reusing a finalized installation, the candidate is
reported unavailable and the working carrier is not replaced.
