// The live updates of a page (see mod.rs): `handlers[event](data)` for each
// message. When the connection ends (e.g. a restart of the bot), it
// connects again.
const live = (path, handlers) => {
  const url = new URL(path, location.href);
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
  const connect = () => {
    const ws = new WebSocket(url);
    ws.onmessage = e => {
      const { event, data } = JSON.parse(e.data);
      handlers[event]?.(data);
    };
    ws.onclose = () => setTimeout(connect, 1000);
  };
  connect();
};
