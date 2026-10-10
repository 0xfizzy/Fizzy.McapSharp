// Each package version has one current documentation directory.
(async () => {
  const script = document.querySelector('script[data-doc-root]');
  const root = new URL(script.dataset.docRoot, document.baseURI);
  const info = await fetch(new URL('doc-info.json', root)).then(r => { if (!r.ok) throw Error(r.status); return r.json(); });
  const bar = document.createElement('div');
  bar.id = 'documentation-version';
  bar.style.cssText = 'padding:.6rem 1rem;border-bottom:1px solid #888;display:flex;gap:1rem;align-items:center';
  const label = document.createElement('span');
  label.textContent = `${info.channel === 'dev' ? 'Development (unreleased)' : `v${info.version}${info.version.includes('-') ? ' (prerelease)' : ''}`} · ${info.docs_commit.slice(0, 12)}`;
  bar.append(label);
  document.body.prepend(bar);
  // Local standalone previews have no shared versions index.
  const shared = new URL('../', root);
  const response = await fetch(new URL('versions.json', shared));
  if (!response.ok) return;
  const versions = await response.json();
  const select = document.createElement('select');
  select.setAttribute('aria-label', 'Documentation version');
  const current = info.channel === 'dev' ? 'dev/' : `${info.version}/`;
  if (versions.dev) select.add(new Option('Development', 'dev/'));
  for (const entry of versions.entries) select.add(new Option(`v${entry.version}`, `${entry.version}/`));
  if (![...select.options].some(option => option.value === current)) select.add(new Option(`${current.replace(/\/$/, '')} (publication pending)`, current));
  select.value = current;
  select.addEventListener('change', async () => {
    if (!select.value) return;
    const targetRoot = new URL(select.value === 'dev/' ? select.value : 'v' + select.value, shared);
    const relative = location.pathname.startsWith(root.pathname) ? location.pathname.slice(root.pathname.length) : 'index.html';
    const target = new URL(relative, targetRoot);
    const exists = await fetch(target, { method: 'HEAD' });
    location.href = exists.ok ? target.href + location.hash : new URL(relative.startsWith('docs/zh-CN/') ? 'docs/zh-CN/index.html' : 'docs/index.html', targetRoot).href;
  });
  bar.append(select);
})().catch(error => console.debug('Documentation version index unavailable:', error));
