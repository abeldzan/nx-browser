// Runs in the "nxbrowser-keys" world in every http(s) frame. Talks to the browser over `nxPasswords`;
// the browser takes the origin from WebKit, never from here, and hands out a password only when one is picked.
(() => {
  if (!/^https?:$/.test(location.protocol)) return;
  const post = (msg) =>
    webkit.messageHandlers.nxPasswords.postMessage(JSON.stringify(msg)).then((reply) => (reply == null ? null : JSON.parse(reply)));
  const shown = (el) => el.getClientRects().length > 0;
  const isPassword = (el) => el instanceof HTMLInputElement && el.type === 'password';
  const isText = (el) => el instanceof HTMLInputElement && ['text', 'email', 'tel'].includes(el.type);
  const scopeOf = (el) => el.form || el.closest('form') || document;
  const passwords = (scope) => [...scope.querySelectorAll('input[type=password]')].filter(shown);
  const usernameFor = (pw) => {
    const scope = scopeOf(pw);
    const marked = [...scope.querySelectorAll('input[autocomplete~=username]')].find(shown);
    if (marked) return marked;
    let found = null;
    for (const el of scope.querySelectorAll('input')) {
      if (isText(el) && shown(el) && el.compareDocumentPosition(pw) & Node.DOCUMENT_POSITION_FOLLOWING) found = el;
    }
    return found;
  };
  // Sign-up and change-password fields want a new password, not a saved one.
  const isNew = (pw) => {
    const hint = (pw.getAttribute('autocomplete') || '').toLowerCase();
    if (hint.includes('new-password')) return true;
    if (hint.includes('current-password')) return false;
    const list = passwords(scopeOf(pw));
    return list.length === 2 || (list.length > 2 && list.indexOf(pw) > 0);
  };

  const valueSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
  const setValue = (el, value) => {
    valueSetter.call(el, value);
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
  };

  let host = null;
  const hide = () => {
    host?.remove();
    host = null;
  };
  const show = (anchor, items) => {
    hide();
    host = document.createElement('nx-passwords');
    const root = host.attachShadow({ mode: 'closed' });
    const style = document.createElement('style');
    style.textContent = `
      .menu { position: fixed; z-index: 2147483647; min-width: 220px; max-width: 360px; box-sizing: border-box; padding: 4px;
        background: Canvas; color: CanvasText; color-scheme: light dark; border: 1px solid rgba(128,128,128,.35);
        border-radius: 8px; box-shadow: 0 6px 24px rgba(0,0,0,.18); font: 13px -apple-system, system-ui, sans-serif; }
      button { display: block; width: 100%; border: 0; background: none; color: inherit; font: inherit; text-align: left;
        padding: 6px 10px; border-radius: 5px; }
      button:hover, button:focus-visible { background: Highlight; color: HighlightText; outline: none; }
      small { display: block; opacity: .65; font-size: 11px; }`;
    const menu = document.createElement('div');
    menu.className = 'menu';
    menu.setAttribute('role', 'listbox');
    const box = anchor.getBoundingClientRect();
    menu.style.left = `${Math.max(4, box.left)}px`;
    menu.style.top = `${box.bottom + 4}px`;
    for (const item of items) {
      const button = document.createElement('button');
      button.setAttribute('role', 'option');
      button.textContent = item.label;
      const hint = document.createElement('small');
      hint.textContent = item.hint;
      button.append(hint);
      // Keep focus in the field, so picking doesn't blur it and close the menu first.
      button.addEventListener('mousedown', (e) => e.preventDefault());
      button.addEventListener('click', () => {
        hide();
        item.run();
      });
      menu.append(button);
    }
    root.append(style, menu);
    document.documentElement.append(host);
  };

  let logins = null;
  const offer = (el) => {
    if (!(el instanceof HTMLInputElement) || !(isPassword(el) || isText(el))) return;
    const pw = isPassword(el) ? el : passwords(scopeOf(el)).find((p) => usernameFor(p) === el);
    if (!pw) return hide();
    const items = [];
    if (isNew(pw)) {
      if (el === pw) {
        items.push({
          label: 'Use strong password',
          hint: 'Saved when you submit',
          run: () => post({ kind: 'generate' }).then((p) => p && passwords(scopeOf(pw)).filter(isNew).forEach((f) => setValue(f, p))),
        });
      }
    } else {
      for (const login of logins || []) {
        items.push({
          label: login.username || '(no username)',
          hint: 'Saved password',
          run: () =>
            post({ kind: 'fill', id: login.id }).then((p) => {
              if (p == null) return;
              const user = usernameFor(pw);
              if (user && login.username) setValue(user, login.username);
              setValue(pw, p);
            }),
        });
      }
    }
    items.length ? show(el, items) : hide();
  };
  const ask = () => {
    if (logins !== null || !passwords(document).length) return;
    logins = [];
    post({ kind: 'form' })
      .then((list) => {
        logins = list || [];
        if (document.activeElement) offer(document.activeElement);
      })
      .catch(() => {});
  };
  let pending = false;
  new MutationObserver(() => {
    if (logins !== null || pending) return;
    pending = true;
    setTimeout(() => {
      pending = false;
      ask();
    }, 300);
  }).observe(document, { childList: true, subtree: true });
  addEventListener('DOMContentLoaded', ask);
  document.addEventListener('focusin', (e) => offer(e.target), true);
  document.addEventListener('click', (e) => e.target !== document.activeElement || offer(e.target), true);
  document.addEventListener('focusout', hide, true);
  document.addEventListener('keydown', (e) => e.key === 'Escape' && hide(), true);
  document.addEventListener('input', (e) => e.isTrusted && hide(), true);
  addEventListener('scroll', hide, { capture: true, passive: true });
  addEventListener('resize', hide);

  // Multi-step sign-ins (username page, then password page in the same document) leave the username behind.
  let remembered = '';
  let lastSent = '';
  const capture = (scope) => {
    const filled = passwords(scope).filter((p) => p.value);
    if (!filled.length) {
      const users = [...scope.querySelectorAll('input')].filter((el) => isText(el) && shown(el) && el.value);
      if (users.length === 1) remembered = users[0].value.trim();
      return;
    }
    const current = filled.find((p) => !isNew(p));
    const fresh = filled.filter(isNew);
    const user = usernameFor(current || fresh[0]);
    const msg = {
      kind: 'submit',
      username: (user && user.value.trim()) || remembered,
      password: current ? current.value : '',
      new_password: fresh.length ? fresh[fresh.length - 1].value : null,
    };
    const key = JSON.stringify(msg);
    if (key === lastSent) return;
    lastSent = key;
    setTimeout(() => (lastSent = ''), 3000);
    post(msg).catch(() => {});
  };
  document.addEventListener('submit', (e) => capture(e.target), true);
  document.addEventListener(
    'click',
    (e) => {
      const button = e.target instanceof Element && e.target.closest('button, input[type=submit], [role=button]');
      if (!button) return;
      const form = button.form || button.closest('form');
      const looksLikeSubmit = button.type === 'submit' || (!form && /log\s?in|sign\s?in|submit|continue|next/i.test(button.textContent || button.value || ''));
      if (looksLikeSubmit) capture(form || document);
    },
    true
  );
  document.addEventListener('keydown', (e) => e.key === 'Enter' && e.target instanceof HTMLInputElement && capture(scopeOf(e.target)), true);
})();
