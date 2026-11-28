# RustDesk QoS adaptation

The configured capture ceiling remains 144 FPS. Capture pacing and encoder quality
continue to use the pinned RustDesk `VIDEO_QOS` controller. The encoder's nominal
FPS remains the configured ceiling so temporary capture throttling does not
reintroduce the nominal-30-FPS encoder budget problem.

Browser decode acknowledgments release the bounded bridge window immediately.
They are not fed directly into the QoS controller: its consecutive-reply rules
and 20-sample baseline were designed around one-second probes, not 144 replies
per second. Each connection keeps its latest fresh decode-delay measurement;
an independent one-second timer feeds that sample to `user_network_delay` once.
No sample is replayed when the screen is idle. No synthetic zero-delay sample
is supplied on connection initialization.

The measurement runs from the SDK source timestamp to the browser decode ACK.
It includes encoding, transport, browser processing and the return path. It is
not pure RTT or the same measurement as native RustDesk `TestDelay`. The SDK's
baseline subtraction and bitrate-first adaptation operate on this measurement.
The ACK follows canvas drawing, not confirmation of physical screen presentation.

While video is outstanding, over two seconds without ACK progress also invokes
`user_delay_response_elapsed`. The timer operates even when no frame or ACK
arrives. Refreshing the reference chain does not reset the stall origin without
ACK progress. With no outstanding video, an idle screen never triggers this
timeout. RustDesk owns reduction factors, bitrate cooldowns and FPS recovery;
the adapter does not implement a competing congestion algorithm.

The existing 18-frame / 32-MiB window, shared display encoders and global SDK QoS
remain. A slow viewer can affect other viewers; this change does not provide
per-viewer encoder isolation or per-frame unreliable transport. 144 FPS is the
healthy-path target, not a weak-network guarantee.

Regression tests include full-rate ACK sampling, idle screens, missing ACKs,
refresh during a stall, and a private instance of the actual pinned SDK QoS
controller reaching 144 FPS, reducing bitrate and FPS, braking on timeout and
recovering. Run the engine's `cargo test --release --locked --lib video_ --
--test-threads=1` with the platform native dependencies configured.

## Windows validation, 2026-11-28

The final adapter passed 17 engine video tests (including the real SDK QoS
controller) and 106 plugin library tests. Both the engine static library and
Desk plugin were rebuilt locally against the verified native dependency archive
from CI run 46030739026. These are local diagnostic binaries, not a published
release or an installed-service upgrade.

On the existing 2560 x 1440, 144 Hz Windows desktop, a 30-second isolated LAN
measurement decoded 144.003 FPS, captured/encoded 144.012 FPS, with a 144.002 FPS
animation source. GPU texture capture and hardware encoding remained active.
The run included a 40 ms viewer pause and had no decoder/chunk errors or decoder
reconfigurations. Evidence is in
`artifacts/hardware-pipeline/lan-ci-local-qos-feedback-1795847895/report.json`.
The report records the exact DLL hash; its staged `plugin.json` records the
adapter source hashes, including the newly added sampler file.

A second run put a TCP tunnel between this same browser and host, limiting the
server-to-browser direction to 5 Mbps from 22 to 45 seconds after the first
connection, then removing the limit. The real SDK quality ratio fell from
13.160 to 0.942 and its FPS target reached 8 during the congestion interval.
After removing the limit, the final nine-second interval decoded 143.856 FPS;
quality was recovering (final ratio 1.363). There were no decoder/chunk errors.
Evidence is in
`artifacts/hardware-pipeline/lan-ci-local-qos-feedback-1795847945/report.json`
and its `service.log` (`qos_trace`). The specialized `shapingPassed` gate passed.
The ordinary whole-run 144-FPS gate is recorded as false and explicitly marked
inapplicable: its average intentionally includes throttled, reduced-FPS video.
This test exercises a bandwidth bottleneck, not random packet loss or a real
cross-network STUN/TURN route. Both runs used the same exact final DLL.

This is same-machine real desktop capture and headless browser decoding. It
does not establish visible-browser presentation throughput or performance
across two physical networks or on other hardware/platforms.
