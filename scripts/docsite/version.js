// Loaded relative to each immutable revision; only the shared index changes.
(async () => {
  const script = document.querySelector('script[data-doc-root]');
  const root = new URL(script.dataset.docRoot, document.baseURI);
  const info = await fetch(new URL('doc-info.json', root)).then(r => { if (!r.ok) throw Error(r.status); return r.json(); });
  const bar = document.createElement('aside');
  bar.setAttribute('aria-label', 'Documentation version');
  bar.style.cssText = 'padding:8px 16px;background:#e8eef5;color:#182433;position:relative;z-index:1000';
  const label = document.createElement('span');
  label.textContent = `${info.version} / ${info.revision === null ? 'dev — unreleased' : 'r' + info.revision} · `;
  bar.append(label);
  const source = document.createElement('a');
  source.href = `https://github.com/0xfizzy/Fizzy.McapSharp/tree/${info.docs_commit}`;
  source.textContent = info.docs_commit.slice(0, 12);
  bar.append(source);
  document.body.prepend(bar);
  // Local standalone previews have no shared versions index.
  const shared = new URL(info.revision === null ? '../' : '../../', root);
  const response = await fetch(new URL('versions.json', shared));
  if (!response.ok) return;
  const versions = await response.json();
  const select = document.createElement('select');
  select.setAttribute('aria-label', 'Switch documentation version');
  const placeholder = new Option('Switch version / 切换版本', '');
  select.add(placeholder);
  for (const entry of versions.entries) select.add(new Option(`${entry.version} / r${entry.current}`, `${entry.version}/r${entry.current}/`));
  if (versions.dev) select.add(new Option('dev — unreleased', 'dev/'));
  select.addEventListener('change', async () => {
    if (!select.value) return;
    const targetRoot = new URL(select.value === 'dev/' ? select.value : 'v' + select.value, shared);
    const relative = location.pathname.startsWith(root.pathname) ? location.pathname.slice(root.pathname.length) : 'index.html';
    const target = new URL(relative, targetRoot);
    const exists = await fetch(target, { method: 'HEAD' });
    location.href = exists.ok ? target.href + location.hash : new URL(relative.startsWith('docs/zh-CN/') ? 'docs/zh-CN/index.html' : 'docs/index.html', targetRoot).href;
  });
  bar.append(' ', select);
})().catch(error => console.debug('Documentation version index unavailable:', error));
