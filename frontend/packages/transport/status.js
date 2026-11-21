// Describe the selected data path, never the signaling connection or requested path.
export function describeTransport(path, pair) {
  if (path === 'websocket') return { label: 'TCP', detail: '网关 WebSocket 转发', direct: false };
  if (!pair) return { label: '检测中', detail: '正在确认实际连接通道', direct: null };
  const relay = pair.local === 'relay' || pair.remote === 'relay';
  const selected = (relay ? pair.relayProtocol : null) || pair.protocol;
  const protocol = selected === 'tls' ? 'tcp' : selected;
  if (protocol !== 'udp' && protocol !== 'tcp')
    return { label: '检测中', detail: '正在确认实际连接协议', direct: !relay };
  const label = protocol === 'udp' ? 'UDP' : 'TCP';
  return {
    label: label + (relay ? '' : ' P2P'),
    detail: relay ? 'TURN 中继转发' : 'WebRTC 直连（STUN / ICE）',
    direct: !relay,
  };
}
