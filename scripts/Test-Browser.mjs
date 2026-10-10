import { chromium } from '@playwright/test';
import { createServer } from 'node:http';
import { readFile, stat, mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import assert from 'node:assert/strict';
const repo = path.resolve(import.meta.dirname, '..');
const site = path.join(repo, 'artifacts/docs/site');
const urlIndex = process.argv.indexOf('--url');
const liveUrl = urlIndex >= 0 ? new URL(process.argv[urlIndex + 1]).href.replace(/\/?$/, '/') : null;
const info = liveUrl
  ? await fetch(new URL('doc-info.json', liveUrl)).then(response => { assert(response.ok, 'Live documentation identity'); return response.json(); })
  : JSON.parse(await readFile(path.join(site, 'doc-info.json'), 'utf8'));
const prefix = '/Fizzy.McapSharp/';
const mounts = ['dev', 'v0.0.0/r0'];
const mime = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.json': 'application/json', '.svg': 'image/svg+xml', '.woff2': 'font/woff2' };
const server = createServer(async (req, res) => {
  try {
    const pathname = decodeURIComponent(new URL(req.url, 'http://localhost').pathname);
    if (pathname === prefix + 'versions.json') {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ entries: [{ version: '0.0.0', current: 0, completed: true }], dev: true })); return;
    }
    const mount = mounts.find(v => pathname.startsWith(`${prefix}${v}/`));
    if (!mount) { res.writeHead(404).end(); return; }
    const relative = pathname.slice(`${prefix}${mount}/`.length) || 'index.html';
    const target = path.resolve(site, relative);
    if (!target.startsWith(site + path.sep)) { res.writeHead(403).end(); return; }
    if (mount !== 'dev' && relative === 'docs/zh-CN/usage.html') { res.writeHead(404).end(); return; }
    if (relative === 'doc-info.json') {
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ ...info, version: mount === 'dev' ? info.version : '0.0.0', revision: mount === 'dev' ? null : 0 })); return;
    }
    const file = (await stat(target)).isDirectory() ? path.join(target, 'index.html') : target;
    res.setHeader('content-type', mime[path.extname(file)] || 'application/octet-stream');
    res.end(await readFile(file));
  } catch { res.writeHead(404).end(); }
});
if (!liveUrl) await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const base = liveUrl || `http://127.0.0.1:${server.address().port}${prefix}dev/`;
const output = path.join(repo, liveUrl ? 'artifacts/docs/browser-live' : 'artifacts/docs/browser');
let browser, page;
try {
  browser = await chromium.launch({ headless: true });
  page = await browser.newPage();
  const errors = [], resources = [];
  page.on('pageerror', error => errors.push(error.message));
  page.on('response', response => { if (['script', 'stylesheet', 'font', 'image'].includes(response.request().resourceType()) && response.status() >= 400) resources.push(response.url()); });
  for (const relative of ['docs/index.html', 'docs/zh-CN/index.html', 'api/Fizzy.McapSharp.McapWriter.html']) {
    const response = await page.goto(base + relative);
    assert.equal(response.status(), 200, relative);
    await page.getByLabel('Switch documentation version').waitFor();
    assert.match(await page.getByLabel('Documentation version', { exact: true }).innerText(), /dev — unreleased/);
  }
  if (!liveUrl) {
    await page.getByLabel('Switch documentation version').selectOption('0.0.0/r0/');
    await page.waitForURL('**/v0.0.0/r0/api/Fizzy.McapSharp.McapWriter.html');
    await page.goto(base + 'docs/zh-CN/usage.html');
    await page.getByLabel('Switch documentation version').selectOption('0.0.0/r0/');
    await page.waitForURL('**/v0.0.0/r0/docs/zh-CN/index.html');
  }
  await page.goto(base + 'docs/index.html');
  await page.locator('article').getByRole('link', { name: '简体中文', exact: true }).click();
  await page.waitForURL('**/docs/zh-CN/index.html');
  await page.locator('article').getByRole('link', { name: 'English', exact: true }).click();
  await page.waitForURL('**/docs/index.html');
  await page.goto(base + 'index.html');
  await page.locator('#search-query').fill('McapWriter');
  const result = page.locator('#search-results a').filter({ hasText: 'McapWriter' }).first();
  await result.waitFor({ state: 'visible' });
  assert.match(await result.getAttribute('href'), /McapWriter/);
  if (await result.getAttribute('target') === '_blank') {
    const opened = page.waitForEvent('popup');
    await result.click();
    page = await opened;
    page.on('pageerror', error => errors.push(error.message));
  } else {
    await result.click();
  }
  await page.waitForURL(url => /\/api\/[^/]*McapWriter[^/]*\.html$/.test(url.pathname));
  await page.getByLabel('Switch documentation version').waitFor();
  assert.deepEqual(errors, []);
  assert.deepEqual(resources, []);
  await mkdir(output, { recursive: true });
  await writeFile(path.join(output, 'result.json'), JSON.stringify({ passed: true, chromium: browser.version(), sourceCommit: info.docs_commit, url: liveUrl, syntheticVersions: !liveUrl }));
  console.log(liveUrl ? 'PASS: live bilingual navigation, API/search and page/resource errors.' : 'PASS: bilingual navigation, API/search, versions, fallback and page/resource errors.');
} catch (error) {
  await mkdir(output, { recursive: true });
  await page?.screenshot({ path: path.join(output, 'failure.png'), fullPage: true });
  await writeFile(path.join(output, 'result.json'), JSON.stringify({ passed: false, error: String(error), sourceCommit: info.docs_commit, url: liveUrl, syntheticVersions: !liveUrl }));
  throw error;
} finally {
  await browser?.close();
  if (!liveUrl) await new Promise(resolve => server.close(resolve));
}
