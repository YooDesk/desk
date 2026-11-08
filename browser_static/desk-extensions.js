// Windows/Linux 端侧能力入口。普通 fetch/WS 与产品会话共用认证和生命周期。
window.createDeskExtensions = function ({ request, api, state }) {
  const el = (id) => document.getElementById(id);
  let caps = {};
  let audioContext;
  let decoder;
  let format;
  let audioEnabled = false;
  let timestamp = 0;
  let generation = 0;
  const files = window.createDeskFiles({ request, api, state });
  const diagnostic = () => (window.__yoodeskDesk ||= {});
  const endpoint = (body, session = state.sessionId) => {
    if (!session) return Promise.reject(new Error("请先连接桌面"));
    return request("POST", `${api}/sessions/${session}/endpoint/`, body);
  };
  const notify = (error) => {
    el("message").textContent = error.message;
    el("message").classList.add("error");
  };
  const audioOutput = window.createYooDeskAudioOutput({
    button: el('audio'), report: (text) => notify(new Error(text)),
    onNeeded() {
      audioEnabled = audioOutput.active;
      audioContext = audioOutput.context;
      if (audioEnabled) configureAudio(); else closeAudio();
    },
  });
  function closeAudio() {
    generation += 1;
    if (decoder && decoder.state !== 'closed') decoder.close();
    decoder = undefined;
    audioOutput.reset();
    audioContext = audioOutput.context;
    timestamp = 0;
  }
  function configureAudio() {
    if (!audioEnabled || !format || !audioContext) return;
    if (decoder && decoder.state !== 'closed') decoder.close();
    const current = ++generation;
    decoder = new AudioDecoder({
      output(data) {
        try {
          if (current !== generation || !audioContext) return;
          const buffer = audioContext.createBuffer(
            data.numberOfChannels,
            data.numberOfFrames,
            data.sampleRate,
          );
          for (let channel = 0; channel < data.numberOfChannels; channel += 1) {
            data.copyTo(buffer.getChannelData(channel), {
              planeIndex: channel,
              format: "f32-planar",
            });
          }
          audioOutput.play(buffer);
          diagnostic().audioDecoded = (diagnostic().audioDecoded || 0) + 1;
          let energy = 0;
          for (const sample of buffer.getChannelData(0))
            energy += sample * sample;
          diagnostic().audioEnergy = (diagnostic().audioEnergy || 0) + energy;
        } finally {
          data.close();
        }
      },
      error(error) {
        diagnostic().audioError = error.message;
        notify(error);
      },
    });
    decoder.configure({
      codec: "opus",
      sampleRate: format.sampleRate,
      numberOfChannels: format.channels,
    });
  }
  el("privacy").addEventListener("click", async () => {
    try {
      const status = await endpoint({ action: "privacy.get" });
      const result = await endpoint({
        action: "privacy.set",
        enabled: !status.enabled,
      });
      el("privacy").textContent = result.enabled ? "关闭隐私屏" : "开启隐私屏";
      el("privacy").title = el("privacy").textContent;
      el("privacy").setAttribute("aria-pressed", String(result.enabled));
      diagnostic().privacyEnabled = result.enabled;
    } catch (error) {
      notify(error);
    }
  });
  return {
    async pause() {
      audioEnabled = audioOutput.active;
      closeAudio();
      const session = state.sessionId;
      const privacy = diagnostic().privacyEnabled;
      await files.pause();
      if (privacy && state.sessionId === session) {
        await endpoint({action: "privacy.set", enabled: false}, session);
        diagnostic().privacyEnabled = false;
        el("privacy").textContent = "开启隐私屏";
        el("privacy").title = el("privacy").textContent;
        el("privacy").setAttribute("aria-pressed", "false");
      }
    },
    updateAvailability() { files.updateAvailability(); },
    applyCapabilities(value) {
      files.applyCapabilities(value);
      caps = value;
      el("audio").hidden = !caps.audio || typeof AudioDecoder === "undefined";
      el("privacy").hidden = !caps.privacy_screen;
      el("files").hidden = !caps.file_transfer;
    },
    consume(parsed, fields) {
      if (parsed.audioFormat) {
        let sampleRate = 0;
        let channels = 0;
        for (const entry of fields(parsed.audioFormat)) {
          if (entry[0] === 1) sampleRate = entry[2];
          if (entry[0] === 2) channels = entry[2];
        }
        if (
          ![8000, 12000, 16000, 24000, 48000].includes(sampleRate) ||
          ![1, 2].includes(channels)
        )
          return;
        if (
          !format ||
          format.sampleRate !== sampleRate ||
          format.channels !== channels
        ) {
          format = { sampleRate, channels };
          configureAudio();
        }
        diagnostic().audioFormat = format;
      }
      if (parsed.audio && decoder && decoder.state === "configured") {
        const packet = fields(parsed.audio).find(
          (entry) => entry[0] === 1 && entry[1] === 2,
        )?.[2];
        if (packet && decoder.decodeQueueSize < 32) {
          decoder.decode(
            new EncodedAudioChunk({ type: "key", timestamp, data: packet }),
          );
          timestamp += 10_000;
        }
      }
      el("privacy").disabled = state.readonly || state.modePending;
    },
    teardown() {
      files.teardown();
      audioEnabled = audioOutput.active;
      closeAudio();
      format = undefined;
      el("privacy").textContent = "开启隐私屏";
      el("privacy").title = "开启隐私屏";
      el("privacy").setAttribute("aria-pressed", "false");
    },
  };
};
