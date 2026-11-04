import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { once } from 'node:events';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { runInNewContext } from 'node:vm';

const requests = [];
let redirect = true;
const server = createServer(async (req, res) => {
  const chunks = [];
  for await (const chunk of req) chunks.push(chunk);
  requests.push({ path: req.url, csrf: req.headers['x-csrf-token'], body: Buffer.concat(chunks).toString() });
  if (redirect && req.url !== '/target') res.writeHead(307, { Location: '/target' }).end('redirect');
  else res.writeHead(200, { 'content-type': 'application/json' }).end('{}');
});
server.listen(0, '127.0.0.1');
await once(server, 'listening');
const basePath = `http://127.0.0.1:${server.address().port}`;
try {
  const failures = [];
  for (const name of ['base.html', 'login.html', 'change_password.html', 'pending.html']) {
    const source = readFileSync(join(process.argv[2], name), 'utf8');
    // Execute the actual request expression. The DOM and response UI are irrelevant
    // to whether its network request forwards a password or CSRF header.
    const expression = source.match(/fetch\([\s\S]*?\n\s*\}\)/)?.[0];
    assert.ok(expression, `${name}: shipped fetch expression must exist`);
    const context = { fetch, basePath, url: `${basePath}/admin/pending-writes/id/reject`, body: { password: 'private fixture password' }, cookie: () => 'fixture-csrf', decodeURIComponent, JSON, action: 'reject', reason: { value: 'private rejection reason' } };
    requests.length = 0;
    redirect = true;
    let refused = false;
    try { await runInNewContext(expression, context); } catch { refused = true; }
    if (!refused || requests.length !== 1 || requests[0].path === '/target') failures.push(`${name}: HTTP 307 accepted or forwarded (${requests.length} requests)`);
    requests.length = 0;
    redirect = false;
    assert.equal((await runInNewContext(expression, context)).status, 200, `${name}: direct control`);
    assert.equal(requests.length, 1);
    if (name !== 'base.html') assert.ok(requests[0].body.includes('private'), `${name}: control must carry private data`);
    if (name !== 'login.html') assert.equal(requests[0].csrf, 'fixture-csrf');
  }
  assert.deepEqual(failures, []);
  console.log('PASS: four shipped web requests refuse 307 and accept direct 200');
} finally {
  server.closeAllConnections();
  await new Promise(resolve => server.close(resolve));
}
