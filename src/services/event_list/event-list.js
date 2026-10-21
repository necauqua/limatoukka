const list = document.getElementById('list');
// the time to move the entries by one line
const MOVE_MS = 250;
// the time to count a number up to its new value
const COUNT_MS = 1000;

const shown = root => [...root.children].filter(entry => !entry.classList.contains('leaving'));
// a merge keeps the id of the entry and changes its time
const version = root => shown(root).map(entry => `${entry.dataset.id}@${entry.dataset.at}`).join(' ');
const decimals = value => (value.split('.')[1] ?? '').length;

// counts `to` up from the value that `from` shows
const countUp = (from, to) => {
  const start = from.shownValue ?? Number(from.dataset.value);
  const end = Number(to.dataset.value);
  if (Number.isNaN(start) || start === end) {
    return;
  }
  const places = Math.max(decimals(from.dataset.value), decimals(to.dataset.value));
  const text = to.textContent;
  const began = performance.now();
  const step = now => {
    // a newer update replaced the element
    if (!to.isConnected) {
      return;
    }
    const t = Math.min((now - began) / COUNT_MS, 1);
    if (t === 1) {
      delete to.shownValue;
      to.textContent = text;
      return;
    }
    to.shownValue = start + (end - start) * (1 - (1 - t) ** 3);
    to.textContent = to.shownValue.toFixed(places);
    requestAnimationFrame(step);
  };
  step(began);
};

// each list has all the shown entries, the first one is sent on (re)connect
const update = html => {
  const next = document.createElement('div');
  next.innerHTML = html;
  if (version(next) === version(list)) {
    return;
  }
  const old = new Map(shown(list).map(entry => [entry.dataset.id, entry]));
  // where the entries are now, also in the middle of a move
  const oldTops = new Map([...old].map(([id, entry]) => [id, entry.getBoundingClientRect().top]));

  // the entries that drop out of the list stay below the others until they
  // move down off the list
  const nextIds = new Set([...next.children].map(entry => entry.dataset.id));
  const leaving = [...old.values()]
    .filter(entry => !nextIds.has(entry.dataset.id))
    .map(entry => {
      const copy = entry.cloneNode(true);
      copy.classList.add('leaving');
      return copy;
    });
  const entries = [...next.children, ...leaving];
  list.replaceChildren(...entries);

  // each entry moves from its old place to its new place; a new entry comes
  // from one line above its place
  const line = entries[0]?.offsetHeight ?? 0;
  for (const entry of entries) {
    const id = entry.dataset.id;
    const top = entry.getBoundingClientRect().top;
    const dy = (oldTops.get(id) ?? top - line) - top;
    const isLeaving = entry.classList.contains('leaving');
    if (dy !== 0) {
      // a merged entry that moves up goes over the others
      entry.classList.toggle('rising', dy > 0 && old.has(id) && !isLeaving);
      const move = entry.animate(
        [{ transform: `translateY(${dy}px)` }, { transform: 'none' }],
        { duration: MOVE_MS, easing: 'linear' },
      );
      move.onfinish = () => isLeaving ? entry.remove() : entry.classList.remove('rising');
    } else if (isLeaving) {
      entry.remove();
    }

    const was = old.get(id);
    if (was && !isLeaving) {
      const wasNumbers = was.querySelectorAll('[data-value]');
      entry.querySelectorAll('[data-value]').forEach((number, i) => {
        if (wasNumbers[i]) {
          countUp(wasNumbers[i], number);
        }
      });
    }
  }
};
live('/event-list/events', { list: update });
