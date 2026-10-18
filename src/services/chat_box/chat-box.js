const log = document.getElementById('log');
// the number of messages that the server keeps
const KEPT = Number(log.dataset.kept);

const loaded = img => img.complete && img.naturalWidth;
// the height of the spacer of a gigantified emote, in em (see chat-box.css)
const GIANT = 7;

// The emote image is as large as it fits in the box of the spacer image
// and its padding (as in the Streamlabs chat box). The layers get this size,
// so that the zero-width emotes are as high as the emote and the modifiers
// change the image only. An emote or zero-width emote that is wider than the
// box gets space at the sides, so that it does not cover the text next to it.
const sizeEmote = emote => {
  const spacers = emote.querySelectorAll('img.spacer');
  const spacer = spacers[0];
  const images = [spacer, ...emote.querySelectorAll('img.zw')];
  const waiting = images.filter(img => !loaded(img));
  if (waiting.length) {
    // again when the next one is loaded
    waiting.forEach(img => img.addEventListener('load', () => sizeEmote(emote), { once: true }));
    if (!loaded(spacer)) {
      return;
    }
  }
  const box = emote.getBoundingClientRect();
  if (!box.height) {
    return; // not shown
  }
  const spacerBox = spacer.getBoundingClientRect();
  // a wide emote has two spacers; the image fits the box of one
  const width = box.width - spacerBox.width * (spacers.length - 1);
  const scale = Math.min(width / spacer.naturalWidth, box.height / spacer.naturalHeight);
  const height = spacer.naturalHeight * scale;
  const imageWidth = spacer.naturalWidth * scale * spacers.length;
  // in em, so that the sizes follow a later change of the font size (e.g.
  // the custom CSS of an OBS browser source, which comes after the load);
  // the spacer is 1em high (GIANT for a gigantified emote), from the same
  // layout as the box
  const em = spacerBox.height / (emote.classList.contains('giant') ? GIANT : 1);
  emote.style.setProperty('--w', `${imageWidth / em}em`);
  emote.style.setProperty('--h', `${height / em}em`);
  emote.classList.add('sized');

  // also a wide emote is wider than its box
  const widest = Math.max(
    imageWidth,
    ...images.slice(1).filter(loaded).map(img => (img.naturalWidth * height) / img.naturalHeight),
  );
  const side = Math.max(0, (widest - box.width) / 2);
  emote.style.marginInline = side ? `${side / em}em` : '';
};
const sizeEmotes = root => root.querySelectorAll('.emote').forEach(sizeEmote);

const parse = html => {
  const template = document.createElement('template');
  template.innerHTML = html;
  return [...template.content.children];
};
const byAttr = (name, value) => log.querySelectorAll(`[${name}="${CSS.escape(value)}"]`);

sizeEmotes(log);

const events = new EventSource('/chat/events');
// the full list, sent on each (re)connect
events.addEventListener('reset', e => {
  log.replaceChildren(...parse(e.data));
  sizeEmotes(log);
});
events.addEventListener('message', e => {
  for (const message of parse(e.data)) {
    byAttr('data-id', message.dataset.id).forEach(old => old.remove());
    log.append(message);
    sizeEmotes(message);
  }
  while (log.children.length > KEPT) {
    log.firstElementChild.remove();
  }
});
// a shown message again, with the chatter data that came after it
events.addEventListener('replace', e => {
  for (const message of parse(e.data)) {
    const [old] = byAttr('data-id', message.dataset.id);
    if (old) {
      old.replaceWith(message);
      sizeEmotes(message);
    }
  }
});
events.addEventListener('delete', e => {
  byAttr('data-id', e.data).forEach(message => message.classList.add('deleted'));
});
events.addEventListener('delete-user', e => {
  byAttr('data-user-id', e.data).forEach(message => message.classList.add('deleted'));
});
events.addEventListener('clear', () => log.replaceChildren());
