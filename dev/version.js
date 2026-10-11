// Each package version has one current documentation directory.
(async () => {
  const script = document.querySelector('script[data-doc-root]');
  const root = new URL(script.dataset.docRoot, document.baseURI);
  const info = await fetch(new URL('doc-info.json', root)).then(r => { if (!r.ok) throw Error(r.status); return r.json(); });
  const bar = document.createElement('div');
  bar.id = 'documentation-version';
  bar.className = 'documentation-version';
  const label = document.createElement('span');
  label.textContent = `${info.channel === 'dev' ? 'Development (unreleased)' : `v${info.version}${info.version.includes('-') ? ' (prerelease)' : ''}`} · ${info.docs_commit.slice(0, 12)}`;
  bar.append(label);
  const select = document.createElement('select');
  select.setAttribute('aria-label', 'Documentation version');
  const current = info.channel === 'dev' ? 'dev/' : `${info.version}/`;
  select.add(new Option(info.channel === 'dev' ? 'Development' : `v${info.version}`, current));
  select.disabled = true;
  bar.append(select);
  document.body.prepend(bar);
  // Local standalone previews have no shared versions index.
  const shared = new URL('../', root);
  const response = await fetch(new URL('versions.json', shared));
  if (!response.ok) { bar.title = 'Documentation version list unavailable'; return; }
  const versions = await response.json();
  select.replaceChildren();
  if (versions.dev) select.add(new Option('Development', 'dev/'));
  for (const entry of versions.entries) select.add(new Option(`v${entry.version}`, `${entry.version}/`));
  if (![...select.options].some(option => option.value === current)) select.add(new Option(`${current.replace(/\/$/, '')} (publication pending)`, current));
  select.value = current;
  select.disabled = false;
  select.addEventListener('change', async () => {
    if (!select.value) return;
    select.disabled = true;
    try {
      const targetRoot = new URL(select.value === 'dev/' ? select.value : 'v' + select.value, shared);
      const relative = location.pathname.startsWith(root.pathname) ? location.pathname.slice(root.pathname.length) : 'index.html';
      let target = new URL(relative, targetRoot);
      let response = await fetch(target);
      if (response.status === 404) {
        const fallback = relative.startsWith('api/') ? 'api/Fizzy.McapSharp.html' :
          relative.startsWith('docs/zh-CN/') ? 'docs/zh-CN/index.html' : 'docs/index.html';
        target = new URL(fallback, targetRoot);
        response = await fetch(target);
        if (response.status === 404) {
          target = new URL('index.html', targetRoot);
          response = await fetch(target);
        }
      }
      if (!response.ok) throw Error(`Documentation page unavailable: ${response.status}`);
      const page = new DOMParser().parseFromString(await response.text(), 'text/html');
      const anchor = decodeURIComponent(location.hash.slice(1));
      if (anchor && page.getElementById(anchor)) target.hash = location.hash;
      location.assign(target.href);
    } catch (error) {
      select.value = current;
      bar.title = String(error);
    } finally { select.disabled = false; }
  });
})().catch(error => console.debug('Documentation version index unavailable:', error));
