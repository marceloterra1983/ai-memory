import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { readFileSync, writeFileSync, readdirSync, mkdirSync, unlinkSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [modulePath, root] = process.argv.slice(2);
const spool = join(root, 'data', 'hook-spool');
mkdirSync(spool, { recursive: true });
const files = () => readdirSync(spool).filter(n => n.endsWith('.json'));
const failures = [];
const check = (ok, label) => { if (!ok) failures.push(label); };
let redirect = true;
const requests = [];
const server = createServer(async (req, res) => {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  requests.push({ path: new URL(req.url, 'http://local').pathname, auth: req.headers.authorization, body: Buffer.concat(chunks).toString() });
  if (redirect && !req.url.startsWith('/target')) {
    res.writeHead(307, { Location: '/target' }).end('gateway');
  } else {
    res.end(req.url.startsWith('/mcp') ? JSON.stringify({ result: { ok: true } }) : req.url.startsWith('/target') ? 'foreign handoff' : 'legitimate handoff');
  }
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const origin = `http://127.0.0.1:${server.address().port}`;
writeFileSync(modulePath, readFileSync(modulePath, 'utf8').replaceAll('http://redirect-fixture.invalid', origin));
try {
  let hooks = await import(pathToFileURL(modulePath).href);
  if (hooks.initTest) hooks = await hooks.initTest(root);
  const payload = { sessionID: 'dummy-session', cwd: root, prompt: 'private test event' };
  const replay = () => requests.filter(r => r.path === '/target');
  const checkBlocked = label => {
    check(replay().length === 0, `${label}: redirect target received ${replay().length} request(s), Authorization=${replay().some(r => r.auth === 'Bearer test-bearer')}`);
    requests.length = 0;
  };
  hooks.postHook('stop', payload);
  await hooks.drainHookQueueForDispose();
  checkBlocked('POST');
  check(files().length === 1, 'POST: 307 must spool the undelivered event');
  for (const name of files()) unlinkSync(join(spool, name));

  const handoff = await hooks.testHandoff(root);
  checkBlocked('handoff');
  check(handoff === undefined, 'handoff: redirect body must not be injected');

  const entry = { url: `${origin}/hook`, body: JSON.stringify(payload), token: 'test-bearer', created_ms: Date.now(), attempts: 0, auth_mode: 'static' };
  const file = join(spool, '0000000000001-1-0000000000000001.json');
  const saved = JSON.stringify(entry);
  writeFileSync(file, saved);
  await hooks.drainHookSpool();
  checkBlocked('spool drain');
  check(files().length === 1 && readFileSync(file, 'utf8') === saved, 'spool drain: 307 must retain the exact event');
  if (hooks.testMcp) {
    let refused = false;
    try { await hooks.testMcp(); } catch { refused = true; }
    checkBlocked('MCP');
    check(refused, 'MCP: 307 must fail');
  }

  redirect = false;
  hooks.postHook('stop', payload);
  await hooks.drainHookQueueForDispose();
  check(files().length === 1, 'control: direct POST must not spool a new event');
  check(await hooks.testHandoff(root) === 'legitimate handoff', 'control: direct handoff must still inject');
  await hooks.drainHookSpool();
  check(files().length === 0, 'control: direct 200 must drain the entry');
  check(requests.some(r => r.path === '/hook' && r.auth === 'Bearer test-bearer' && r.body.includes('private test event')), 'control: configured route must receive the event and authorization');
  if (hooks.testMcp) check((await hooks.testMcp()).ok === true, 'control: direct MCP must work');
  assert.deepEqual(failures, []);
  console.log('PASS: POST, handoff and spool drain refuse 307; direct delivery works');
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
