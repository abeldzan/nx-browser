// Element picker for screenshots, run with callAsyncJavaScript. Hovering outlines an element and a click selects it;
// the bar then walks to the outer or inner element, and for one that doesn't fit on screen picks the full element or just
// its visible part. Resolves "x,y,width,height" (document CSS px) on Capture, null on cancel.
return await new Promise((resolve) => {
  // Custom tag names, so page selectors like `div { … }` don't restyle them.
  const outline = document.createElement('nx-shot-outline');
  outline.style.cssText = 'position:fixed;z-index:2147483646;pointer-events:none;display:none;box-sizing:border-box;border:2px dashed #3b82f6;background:rgba(59,130,246,.12)';
  const host = document.createElement('nx-shot-bar');
  host.style.cssText = 'position:fixed;z-index:2147483647;display:none';
  const root = host.attachShadow({ mode: 'closed' });
  root.innerHTML = `<style>
    .bar { display: flex; gap: 2px; align-items: center; height: 34px; box-sizing: border-box; padding: 0 4px; border-radius: 10px;
      background: rgba(24, 24, 27, .96); color: #fafafa; font: 500 12px/1 -apple-system, system-ui, sans-serif;
      box-shadow: 0 0 0 1px rgba(255,255,255,.08), 0 8px 24px rgba(0,0,0,.35); white-space: nowrap; }
    .tag { padding: 0 8px; color: #a1a1aa; font-weight: 400; max-width: 220px; overflow: hidden; text-overflow: ellipsis;
      font-variant-numeric: tabular-nums; }
    .sep { width: 1px; height: 16px; margin: 0 4px; background: rgba(255,255,255,.12); }
    button { all: unset; display: inline-flex; align-items: center; height: 26px; padding: 0 10px; border-radius: 6px; cursor: pointer; }
    button:hover { background: rgba(255,255,255,.1); }
    button:disabled { opacity: .35; cursor: default; background: none; }
    .go { background: #3b82f6; padding: 0 12px; } .go:hover { background: #2563eb; }
    .close { padding: 0 8px; color: #a1a1aa; } .close:hover { color: #fafafa; }
    .fit { display: inline-flex; padding: 2px; gap: 2px; border-radius: 8px; background: rgba(0,0,0,.4);
      box-shadow: inset 0 0 0 1px rgba(255,255,255,.06); }
    .fit button { height: 22px; padding: 0 10px; color: #a1a1aa; }
    .fit button:hover { background: none; color: #fafafa; }
    .fit button[aria-pressed=true] { background: #3f3f46; color: #fafafa; box-shadow: 0 1px 2px rgba(0,0,0,.4), inset 0 0 0 1px rgba(255,255,255,.08); }
    .fit-group { display: contents; }
  </style><div class="bar"><span class="tag"></span><span class="sep"></span>
    <button data-do="outer" title="Select the outer element">↑ Outer</button>
    <button data-do="inner" title="Select the inner element">↓ Inner</button>
    <span class="fit-group"><span class="sep"></span><span class="fit" role="group" aria-label="Capture area">
      <button data-do="full" title="Scroll to capture the whole element">Full</button>
      <button data-do="visible" title="Capture only the part on screen">Visible</button>
    </span></span><span class="sep"></span>
    <button data-do="capture" class="go" title="Capture (Return)">Capture</button>
    <button data-do="cancel" class="close" title="Cancel (Esc)">✕</button></div>`;
  document.documentElement.append(outline, host);
  const tag = root.querySelector('.tag');
  const outerButton = root.querySelector('[data-do=outer]');
  const innerButton = root.querySelector('[data-do=inner]');
  const fit = root.querySelector('.fit-group');
  // Capture only the on-screen part of an element that doesn't fit; kept across selections.
  let visibleOnly = false;
  let hovered = null;
  let selected = null;
  // Elements left by "Outer", so "Inner" walks back down the same way.
  let inner = [];

  const place = (el) => {
    const r = el.getBoundingClientRect();
    Object.assign(outline.style, { display: 'block', left: r.left + 'px', top: r.top + 'px', width: r.width + 'px', height: r.height + 'px' });
    return r;
  };
  // Outer/inner skip elements that look like no step: zero-size ones (display: contents, hidden) and same-box wrappers.
  const box = (el) => el.getBoundingClientRect();
  const empty = (el) => { const r = box(el); return r.width < 1 || r.height < 1; };
  const same = (a, b) => {
    const [r, s] = [box(a), box(b)];
    return Math.abs(r.left - s.left) < 1 && Math.abs(r.top - s.top) < 1 && Math.abs(r.width - s.width) < 1 && Math.abs(r.height - s.height) < 1;
  };
  const outerOf = (el) => {
    let up = el.parentElement;
    while (up && (empty(up) || same(up, el))) up = up.parentElement;
    return up;
  };
  // Visible children, looking through zero-size ones.
  const kids = (el) => [...el.children].flatMap((child) => (empty(child) ? kids(child) : [child]));
  const innerOf = (el) => {
    let down = kids(el)[0];
    while (down && same(down, el)) down = kids(down)[0];
    return down || null;
  };
  const describe = (el) => {
    const r = el.getBoundingClientRect();
    const name = el.tagName.toLowerCase() + (el.id ? '#' + el.id : el.classList[0] ? '.' + el.classList[0] : '');
    return `${name}  ${Math.round(r.width)}×${Math.round(r.height)}`;
  };
  const viewport = () => {
    const e = document.documentElement;
    return { width: Math.min(e.clientWidth, innerWidth), height: Math.min(e.clientHeight, innerHeight) };
  };
  const offScreen = (r) => {
    const view = viewport();
    return r.left < 0 || r.top < 0 || r.right > view.width || r.bottom > view.height;
  };
  const show = () => {
    const r = place(selected);
    outline.style.borderStyle = 'solid';
    tag.textContent = describe(selected);
    outerButton.disabled = !outerOf(selected);
    innerButton.disabled = !inner.length && !innerOf(selected);
    fit.style.display = offScreen(r) ? 'contents' : 'none';
    root.querySelector('[data-do=full]').setAttribute('aria-pressed', !visibleOnly);
    root.querySelector('[data-do=visible]').setAttribute('aria-pressed', visibleOnly);
    host.style.display = 'block';
    const bar = host.getBoundingClientRect();
    // Under the selection, else above it, else inside its top edge.
    const top = r.bottom + 8 + bar.height <= innerHeight ? r.bottom + 8 : r.top - 8 - bar.height >= 0 ? r.top - 8 - bar.height : Math.max(8, r.top + 8);
    host.style.top = Math.min(top, innerHeight - bar.height - 8) + 'px';
    host.style.left = Math.max(8, Math.min(r.left, innerWidth - bar.width - 8)) + 'px';
  };
  const select = (el) => {
    selected = el;
    show();
  };

  const inBar = (e) => e.composedPath().includes(host);
  const swallow = (e) => {
    if (inBar(e)) return;
    e.preventDefault();
    e.stopPropagation();
  };
  const move = (e) => {
    if (selected || inBar(e)) return;
    hovered = e.target;
    place(hovered);
  };
  const click = (e) => {
    if (inBar(e)) return;
    swallow(e);
    inner = [];
    select(e.target);
  };
  const capture = () => {
    let { left, top, right, bottom } = selected.getBoundingClientRect();
    if (visibleOnly) {
      const view = viewport();
      [left, top, right, bottom] = [Math.max(left, 0), Math.max(top, 0), Math.min(right, view.width), Math.min(bottom, view.height)];
    }
    finish([left + scrollX, top + scrollY, right - left, bottom - top].join(','));
  };
  const key = (e) => {
    if (e.key === 'Escape') finish(null);
    else if (e.key === 'Enter' && selected) capture();
    else return;
    e.preventDefault();
    e.stopPropagation();
  };
  const follow = () => {
    if (selected) show();
    else if (hovered) place(hovered);
  };
  root.addEventListener('click', (e) => {
    e.stopPropagation();
    const action = e.target.closest('button:not(:disabled)')?.dataset.do;
    if (action === 'outer') {
      inner.push(selected);
      select(outerOf(selected));
    } else if (action === 'inner') {
      select(inner.pop() || innerOf(selected));
    } else if (action === 'full' || action === 'visible') {
      visibleOnly = action === 'visible';
      show();
    } else if (action === 'capture') {
      capture();
    } else if (action === 'cancel') {
      finish(null);
    }
  });

  const pointer = ['mousedown', 'mouseup', 'pointerdown', 'pointerup'];
  const finish = (rect) => {
    removeEventListener('mousemove', move, true);
    removeEventListener('click', click, true);
    removeEventListener('keydown', key, true);
    removeEventListener('scroll', follow, true);
    removeEventListener('resize', follow);
    for (const type of pointer) removeEventListener(type, swallow, true);
    outline.remove();
    host.remove();
    // Two frames, so the outline and bar are gone from screen before the snapshot.
    requestAnimationFrame(() => requestAnimationFrame(() => resolve(rect)));
  };
  addEventListener('mousemove', move, true);
  addEventListener('click', click, true);
  addEventListener('keydown', key, true);
  addEventListener('scroll', follow, true);
  addEventListener('resize', follow);
  for (const type of pointer) addEventListener(type, swallow, true);
});
