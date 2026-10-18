const list = document.getElementById('list');
const shown = root => [...root.children].filter(entry => !entry.classList.contains('leaving'));
const ids = root => shown(root).map(entry => entry.dataset.id).join(' ');

// each message has all the shown entries, the first one is sent on (re)connect
new EventSource('/event-list/events').onmessage = e => {
  const next = document.createElement('div');
  next.innerHTML = e.data;
  if (ids(next) === ids(list)) {
    return;
  }
  // the entries that drop out of the list stay below the others until they
  // slide down off the list
  const nextIds = new Set([...next.children].map(entry => entry.dataset.id));
  const leaving = shown(list)
    .filter(entry => !nextIds.has(entry.dataset.id))
    .map(entry => {
      const copy = entry.cloneNode(true);
      copy.classList.add('leaving');
      copy.addEventListener('animationend', () => copy.remove());
      return copy;
    });
  const entries = [...next.children, ...leaving];
  for (const entry of entries) {
    entry.classList.add('slide');
  }
  list.replaceChildren(...entries);
};
