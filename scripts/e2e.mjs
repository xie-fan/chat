// E2E 真实用户旅程测试 —— 直接打运行中的部署（默认 docker :28080）
// 用法:
//   E2E_ADMIN_USER=... E2E_ADMIN_PASS=... node scripts/e2e.mjs
//   node scripts/e2e.mjs phase2     # phase2: 重启后持久化验证（读 .e2e-state.json，含 bot）
// 环境: BASE_URL=http://localhost:28080
// 前提: 库里已有管理员。未提供 E2E_ADMIN_USER、且脚本注册的第一个用户成了管理员时退出 2。
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash, randomUUID } from 'node:crypto';
import { deflateSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const BASE = (process.env.BASE_URL || 'http://localhost:28080') + '/v1';
const WS_BASE = BASE.replace(/^http/, 'ws');
const STATE = join(dirname(fileURLToPath(import.meta.url)), '.e2e-state.json');
const PHASE = process.argv[2] || 'phase1';

let pass = 0, fail = 0;
const ok = (name, cond, extra = '') => {
  if (cond) { pass++; console.log(`  ✓ ${name}`); }
  else { fail++; console.log(`  ✗ ${name} ${extra}`); }
};

async function api(token, method, path, body) {
  const res = await fetch(BASE + path, {
    method,
    headers: {
      'content-type': 'application/json',
      ...(token ? { authorization: `Bearer ${token}` } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  let json = null;
  try { json = await res.json(); } catch {}
  return { status: res.status, json };
}

class Ws {
  constructor() { this.queue = []; this.waiters = []; }
  async connect(token, opts = {}) {
    const { json } = await api(token, 'GET', '/ws/ticket');
    const role = opts.role ? `&role=${encodeURIComponent(opts.role)}` : '';
    this.ws = new WebSocket(`${WS_BASE}/ws?ticket=${json.ticket}${role}`);
    this.ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      const i = this.waiters.findIndex((w) => w.pred(m));
      if (i >= 0) { const w = this.waiters.splice(i, 1)[0]; clearTimeout(w.t); w.res(m); }
      else this.queue.push(m);
    };
    await new Promise((res, rej) => { this.ws.onopen = res; this.ws.onerror = rej; });
  }
  send(o) { this.ws.send(JSON.stringify(o)); }
  close() { try { this.ws?.close(); } catch { /* 已经关上 */ } }
  waitFor(pred, ms = 8000) {
    const i = this.queue.findIndex(pred);
    if (i >= 0) return Promise.resolve(this.queue.splice(i, 1)[0]);
    return new Promise((res, rej) => {
      const t = setTimeout(() => rej(new Error('ws frame timeout')), ms);
      this.waiters.push({ pred, res, t });
    });
  }
  waitType(type, extra = () => true, ms) {
    return this.waitFor((m) => m.type === type && extra(m), ms);
  }
}

const rid = Math.random().toString(36).slice(2, 8);
const mk = (n) => ({ username: `e2e${rid}${n}`, password: 'pass123456' });

// PNG CRC-32（ISO 3309），只用于手写测试图片
function crc32(buf) {
  let c = 0xffffffff;
  for (let i = 0; i < buf.length; i++) {
    c ^= buf[i];
    for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
  }
  return (c ^ 0xffffffff) >>> 0;
}

function pngChunk(type, data) {
  const body = Buffer.concat([Buffer.from(type, 'ascii'), data]);
  const out = Buffer.alloc(12 + data.length);
  out.writeUInt32BE(data.length, 0);
  body.copy(out, 4);
  out.writeUInt32BE(crc32(body), 8 + data.length);
  return out;
}

// 8-bit RGB PNG。最长边 > 480 时服务端会生成 JPEG 缩略图。
function pngRgb(width, height, rgb = [12, 80, 160]) {
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const stride = 1 + width * 3;
  const raw = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y++) {
    const row = y * stride;
    raw[row] = 0;
    for (let x = 0; x < width; x++) {
      const o = row + 1 + x * 3;
      raw[o] = rgb[0];
      raw[o + 1] = rgb[1];
      raw[o + 2] = rgb[2];
    }
  }
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    pngChunk('IHDR', ihdr),
    pngChunk('IDAT', deflateSync(raw)),
    pngChunk('IEND', Buffer.alloc(0)),
  ]);
}

// 1×1 PNG，头像只认魔数
const TINY_PNG = Buffer.from([
  0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
  0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
  0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08, 0xd7, 0x63, 0xf8,
  0xcf, 0xc0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x05, 0xfe, 0x02, 0xfe, 0x00, 0x00,
  0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
]);

async function uploadMultipart(token, method, path, fields, file) {
  const fd = new FormData();
  for (const [k, v] of Object.entries(fields)) fd.append(k, v);
  fd.append('file', new Blob([file.bytes], { type: file.type }), file.name);
  const res = await fetch(BASE + path, {
    method,
    headers: { authorization: `Bearer ${token}` },
    body: fd,
  });
  let json = null;
  try { json = await res.json(); } catch {}
  return { status: res.status, json, res };
}

async function phase1() {
  const [A, B, C] = [mk('a'), mk('b'), mk('c')];
  const attachments = [];
  console.log(`users: ${A.username} ${B.username} ${C.username}`);

  // --- 注册/登录/资料 ---
  for (const u of [A, B, C]) {
    const r = await api(null, 'POST', '/auth/register', { username: u.username, password: u.password });
    ok(`register ${u.username}`, r.status === 200 && r.json.access_token, `got ${r.status}`);
    Object.assign(u, r.json);
    // 空库时第一个注册用户就是管理员，「非管理员 403」会整批误报。先停下来。
    if (u === A && !process.env.E2E_ADMIN_USER && u.user?.role === 'admin') {
      console.error(`
E2E 停止（退出码 2）：未设置 E2E_ADMIN_USER，且当前库还没有其他用户。
本脚本注册的第一个用户 ${A.username} 已自动成为管理员。
继续跑会让「非管理员访问后台应 403」整批误报失败。

请先注册一个管理员，再执行：
  E2E_ADMIN_USER=<管理员用户名> E2E_ADMIN_PASS=<密码> BASE_URL=... node scripts/e2e.mjs
`);
      process.exit(2);
    }
  }
  {
    const r = await api(null, 'POST', '/auth/login', { username: A.username, password: A.password });
    ok('login alice', r.status === 200 && r.json.user.username === A.username);
    Object.assign(A, r.json);
  }
  {
    const r = await api(A.access_token, 'PATCH', '/users/me', { display_name: 'AliceA' });
    ok('update display_name', r.status === 200 && r.json.display_name === 'AliceA');
  }
  {
    const r = await api(null, 'POST', '/auth/login', { username: A.username, password: 'wrong' });
    ok('wrong password rejected', r.status === 401 || r.status === 403);
  }

  // --- 好友流 ---
  const reqRes = await api(A.access_token, 'POST', '/friends/requests', { username: B.username, message: 'hi' });
  ok('friend request A->B', reqRes.status === 200 || reqRes.status === 201, `got ${reqRes.status}`);
  {
    const r = await api(B.access_token, 'GET', '/friends/requests');
    const incoming = (r.json.incoming || []).find((x) => x.user.username === A.username);
    ok('B sees incoming request', !!incoming);
    const acc = await api(B.access_token, 'POST', `/friends/requests/${incoming.id}/accept`);
    ok('B accepts', acc.status === 200 || acc.status === 204);
  }
  {
    const r = await api(A.access_token, 'GET', '/friends');
    ok('A friends list has B', (r.json || []).some((f) => f.user.username === B.username));
    const convs = await api(A.access_token, 'GET', '/conversations');
    const dm = (convs.json || []).find((c) => c.kind === 'dm' && c.peer?.username === B.username);
    ok('DM conversation auto-created', !!dm);
    A.dmId = dm.id; B.dmId = dm.id;
  }
  {
    const r = await api(A.access_token, 'GET', `/users/search?q=${B.username.slice(0, 6)}`);
    ok('user search partial match', r.status === 200 && r.json.some((u) => u.id === B.user.id), `got ${r.status}`);
    const r2 = await api(A.access_token, 'GET', `/users/search?q=${encodeURIComponent(B.user.display_name)}`);
    ok('user search by display_name', r2.json.some((u) => u.id === B.user.id));
  }

  // --- 非好友隔离 ---
  {
    const r = await api(C.access_token, 'GET', `/conversations/${A.dmId}/messages`);
    ok('non-member cannot read DM', r.status === 403 || r.status === 404, `got ${r.status}`);
    const d = await api(C.access_token, 'GET', `/conversations/${A.dmId}`);
    ok('non-member cannot view DM detail', d.status === 403 || d.status === 404, `got ${d.status}`);
  }

  // --- WS 连接/presence ---
  const wsa = new Ws(), wsb = new Ws();
  await wsa.connect(A.access_token);
  await wsa.waitType('presence_init');
  await wsb.connect(B.access_token);
  await wsb.waitType('presence_init');
  {
    const p = await wsa.waitType('presence', (m) => m.user_id === B.user.id && m.online === true);
    ok('A sees B online', !!p);
  }

  // --- 私聊消息/ack/typing/read ---
  wsa.send({ type: 'send_message', conversation_id: A.dmId, client_msg_id: 'm1', content: 'hi bob' });
  {
    const ack = await wsa.waitType('message_ack');
    ok('A gets message_ack seq=1', ack.message.seq === 1 && ack.client_msg_id === 'm1');
    const msg = await wsb.waitType('message', (m) => m.message.conversation_id === A.dmId);
    ok('B receives message', msg.message.content === 'hi bob');
  }
  wsb.send({ type: 'typing', conversation_id: B.dmId });
  {
    const t = await wsa.waitType('typing', (m) => m.user.id === B.user.id);
    ok('A sees B typing', !!t);
  }
  wsb.send({ type: 'read', conversation_id: B.dmId, seq: 1 });
  {
    const rs = await wsb.waitType('read_sync', (m) => m.seq === 1);
    ok('B gets read_sync', !!rs);
  }

  // --- 非成员 WS 发送被拒 ---
  const wsc = new Ws();
  await wsc.connect(C.access_token);
  await wsc.waitType('presence_init');
  wsc.send({ type: 'send_message', conversation_id: A.dmId, content: 'sneaky' });
  {
    try {
      const e = await wsc.waitType('error', () => true, 5000);
      ok('non-member WS send rejected', !!e, '');
    } catch { ok('non-member WS send rejected', false, 'no error frame'); }
  }
  wsc.close();

  // --- 群聊 ---
  const g = await api(A.access_token, 'POST', '/conversations/group', { name: '测试群', member_ids: [B.user.id] });
  ok('create group with friend', g.status === 200 || g.status === 201, `got ${g.status}`);
  const gid = g.json.id;
  {
    const bad = await api(A.access_token, 'POST', '/conversations/group', { name: 'x', member_ids: [C.user.id] });
    ok('cannot invite non-friend', bad.status === 403 || bad.status === 400, `got ${bad.status}`);
    const detail = await api(A.access_token, 'GET', `/conversations/${gid}`);
    ok('group has 2 members', detail.json.member_count === 2 || detail.json.members.length === 2);
  }
  // B 需要拿到群 conv（invalidate 或重新拉列表）
  await wsb.waitType('invalidate', () => true, 3000).catch(() => {});
  wsb.send({ type: 'send_message', conversation_id: gid, client_msg_id: 'g1', content: 'hello grp' });
  {
    // 建群会先广播一条「创建了群聊」系统消息，要跳过它等 B 的真实消息
    const gm = await wsa.waitType('message', (m) => m.message.conversation_id === gid && m.message.kind !== 'system');
    ok('A receives group message from B', gm.message.content === 'hello grp' && gm.message.sender_id === B.user.id);
  }

  // --- @提及 ---
  wsa.send({ type: 'send_message', conversation_id: gid, client_msg_id: 'mt1', content: `hi @${B.username} 看下` });
  {
    const mm = await wsb.waitType('message', (m) => m.message.conversation_id === gid && m.message.content.includes('@'));
    ok('mention parsed to B id', mm.message.mentions.includes(B.user.id), JSON.stringify(mm.message.mentions));
  }
  {
    const convs = await api(B.access_token, 'GET', '/conversations');
    const grp = convs.json.find((c) => c.id === gid);
    ok('B has_mention flag', grp.has_mention === true, `got ${grp?.has_mention}`);
    wsb.send({ type: 'read', conversation_id: gid, seq: grp.last_seq });
    await wsb.waitType('read_sync', () => true, 4000).catch(() => {});
    const convs2 = await api(B.access_token, 'GET', '/conversations');
    ok('has_mention clears after read', convs2.json.find((c) => c.id === gid)?.has_mention === false);
  }
  {
    const h = await api(A.access_token, 'GET', `/conversations/${gid}/messages?limit=10`);
    const mmsg = h.json.messages.find((m) => m.content.includes('@'));
    ok('mentions persisted in history', mmsg?.mentions?.includes(B.user.id), JSON.stringify(mmsg?.mentions));
  }

  // --- 会话内消息搜索 ---
  {
    const r = await api(A.access_token, 'GET', `/conversations/${gid}/messages/search?q=grp`);
    ok('search finds message', r.status === 200 && r.json.some((m) => m.content === 'hello grp'), `got ${r.status}`);
    const r2 = await api(A.access_token, 'GET', `/conversations/${gid}/messages/search?q=${encodeURIComponent('@' + B.username)}`);
    ok('search finds mention text', r2.json.some((m) => m.mentions.includes(B.user.id)));
    const bad = await api(C.access_token, 'GET', `/conversations/${gid}/messages/search?q=grp`);
    ok('non-member search rejected', bad.status === 403 || bad.status === 404, `got ${bad.status}`);
  }

  // --- 消息撤回（MM1） ---
  {
    // B 发群消息，群主 A 撤回（群主可撤群内他人消息、不受 24h 限）
    wsb.send({ type: 'send_message', conversation_id: gid, client_msg_id: 'rc1', content: '撤回我' });
    const bsent = await wsb.waitType('message_ack', (m) => m.client_msg_id === 'rc1');
    const rec = await api(A.access_token, 'POST', `/messages/${bsent.message.id}/recall`);
    ok('owner recalls member msg', rec.status === 200 && rec.json?.recalled === true
      && rec.json?.content === '消息已撤回' && rec.json?.recalled_by === A.user.id,
      `got ${rec.status} ${JSON.stringify(rec.json)?.slice(0, 180)}`);
    const rf = await wsa.waitType('message_deleted', (m) => m.message.id === bsent.message.id);
    ok('message_deleted carries recall fields', rf.message.recalled === true
      && rf.message.recalled_by === A.user.id && rf.message.recalled_by_name === 'AliceA'
      && rf.message.recalled_attachment_id === undefined, JSON.stringify(rf.message).slice(0, 200));
    // 发送者本人 24h 内撤回（DELETE 别名同语义；重复撤回幂等）
    wsb.send({ type: 'send_message', conversation_id: gid, client_msg_id: 'rc2', content: '我自己撤' });
    const bsent2 = await wsb.waitType('message_ack', (m) => m.client_msg_id === 'rc2');
    const d = await api(B.access_token, 'DELETE', `/messages/${bsent2.message.id}`);
    ok('sender recalls own via DELETE alias', d.status === 200 && d.json?.recalled === true
      && d.json?.recalled_by === B.user.id, `got ${d.status}`);
    const d2 = await api(B.access_token, 'DELETE', `/messages/${bsent2.message.id}`);
    ok('re-recall idempotent', d2.status === 200 && d2.json?.edit_version === d.json?.edit_version,
      `v=${d2.json?.edit_version}`);
    // 附件撤回：公开下载封口，上传者 Bearer 取回，复用重发克隆新行
    const fd = new FormData();
    fd.append('conversation_id', gid);
    fd.append('file', new Blob(['rc-bytes'], { type: 'text/plain' }), 'rc.txt');
    const up = await fetch(`${BASE}/files`, { method: 'POST', headers: { authorization: `Bearer ${A.access_token}` }, body: fd });
    const att = (await up.json()).id;
    wsa.send({ type: 'send_message', conversation_id: gid, client_msg_id: 'rc3', content: '', attachment_id: att });
    const asent = await wsa.waitType('message_ack', (m) => m.client_msg_id === 'rc3');
    const rec3 = await api(A.access_token, 'POST', `/messages/${asent.message.id}/recall`);
    ok('recall response carries recalled_attachment_id for uploader', rec3.status === 200
      && rec3.json?.recalled_attachment_id === att, `got ${rec3.status} ${JSON.stringify(rec3.json)?.slice(0, 180)}`);
    const gone = await fetch(`${BASE}/files/${att}`);
    ok('recalled attachment hidden (404)', gone.status === 404);
    const back = await fetch(`${BASE}/files/${att}/recalled`, { headers: { authorization: `Bearer ${A.access_token}` } });
    ok('uploader fetches recalled blob', back.status === 200 && (await back.text()) === 'rc-bytes'
      && (back.headers.get('cache-control') || '').includes('no-store'), `got ${back.status}`);
    wsa.send({ type: 'send_message', conversation_id: gid, client_msg_id: 'rc4', content: '', reuse_attachment_id: att });
    const resent = await wsa.waitType('message_ack', (m) => m.client_msg_id === 'rc4');
    const newAtt = resent.message?.attachment?.id;
    ok('reuse_attachment_id resends clone', !!newAtt && newAtt !== att, JSON.stringify(resent.message?.attachment));
    const dl = await fetch(`${BASE}/files/${newAtt}`);
    ok('reused attachment downloads', dl.status === 200 && (await dl.text()) === 'rc-bytes');
  }

  // --- 离线补发 ---
  wsb.close();
  await new Promise((r) => setTimeout(r, 300));
  for (const [i, c] of ['off1', 'off2'].entries()) {
    wsa.send({ type: 'send_message', conversation_id: A.dmId, client_msg_id: `off${i}`, content: c });
  }
  await wsa.waitType('message_ack', (m) => m.message.seq === 3);
  ok('offline msgs persisted (seq 2,3)', true);
  const wsb2 = new Ws();
  await wsb2.connect(B.access_token);
  await wsb2.waitType('presence_init');
  wsb2.send({ type: 'sync', conversation_id: B.dmId, last_seq: 1 });
  {
    const s = await wsb2.waitType('sync_messages', (m) => m.conversation_id === B.dmId);
    const contents = s.messages.map((m) => m.content);
    ok('B syncs offline msgs', contents.includes('off1') && contents.includes('off2'), JSON.stringify(contents));
  }
  {
    const convs = await api(B.access_token, 'GET', '/conversations');
    const dm = convs.json.find((c) => c.id === B.dmId);
    ok('B unread_count = 2', dm.unread_count === 2, `got ${dm?.unread_count}`);
  }

  // --- 历史分页 ---
  {
    const p1 = await api(A.access_token, 'GET', `/conversations/${A.dmId}/messages?before_seq=3`);
    ok('pagination before_seq', p1.json.messages.every((m) => m.seq < 3) && p1.json.messages.length >= 2);
  }

  // --- 文件/图片消息 ---
  {
    const up = async (blob, name, type) => {
      const fd = new FormData();
      fd.append('conversation_id', A.dmId);
      fd.append('file', new Blob([blob], { type }), name);
      const r = await fetch(`${BASE}/files`, {
        method: 'POST',
        headers: { authorization: `Bearer ${A.access_token}` },
        body: fd,
      });
      return { status: r.status, json: await r.json().catch(() => null) };
    };
    // 文本文件
    const f1 = await up('hello file', 'note.txt', 'text/plain');
    ok('upload file', f1.status === 200 && f1.json.size === 10, `got ${f1.status}`);
    wsa.send({ type: 'send_message', conversation_id: A.dmId, client_msg_id: 'f1', content: '', attachment_id: f1.json.id });
    const fm = await wsa.waitType('message', (x) => x.message.attachment?.id === f1.json.id);
    ok('file msg broadcast w/ attachment', fm.message.kind === 'file' && fm.message.attachment.file_name === 'note.txt', JSON.stringify(fm.message.kind));
    ok('placeholder content', fm.message.content === '[文件] note.txt', fm.message.content);
    const dl = await fetch(`${BASE}/files/${f1.json.id}`);
    ok('download file', dl.status === 200 && (await dl.text()) === 'hello file');
    // 图片 → kind=image
    const png = Uint8Array.from([0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,0,0,0,0]);
    const f2 = await up(png, 'p.png', 'image/png');
    wsa.send({ type: 'send_message', conversation_id: A.dmId, client_msg_id: 'f2', content: '', attachment_id: f2.json.id });
    const im = await wsa.waitType('message', (x) => x.message.attachment?.id === f2.json.id);
    ok('image kind derived from mime', im.message.kind === 'image' && im.message.content === '[图片]', JSON.stringify(im.message.kind));
    for (const [message, bytes] of [[fm.message, Buffer.from('hello file')], [im.message, png]]) {
      attachments.push({
        message_id: message.id,
        attachment_id: message.attachment.id,
        size: bytes.length,
        sha256: createHash('sha256').update(bytes).digest('hex'),
      });
    }
    // B 复用 A 的附件 → 拒
    wsb2.send({ type: 'send_message', conversation_id: B.dmId, client_msg_id: 'f3', content: '', attachment_id: f1.json.id });
    try {
      const e = await wsb2.waitType('error', () => true, 5000);
      ok('attachment reuse rejected', !!e);
    } catch { ok('attachment reuse rejected', false, 'no error frame'); }
    // 非成员上传 → 拒
    const fd2 = new FormData();
    fd2.append('conversation_id', A.dmId);
    fd2.append('file', new Blob(['x']), 'x.txt');
    const bad = await fetch(`${BASE}/files`, { method: 'POST', headers: { authorization: `Bearer ${C.access_token}` }, body: fd2 });
    ok('non-member upload rejected', bad.status === 403 || bad.status === 404, `got ${bad.status}`);
  }

  // --- 群主退群→转让 → 解散 ---
  await api(A.access_token, 'DELETE', `/conversations/${gid}/members/me`);
  {
    const d = await api(B.access_token, 'GET', `/conversations/${gid}`);
    ok('ownership transferred to B', d.json.owner_id === B.user.id, `owner=${d.json.owner_id}`);
    const dis = await api(B.access_token, 'DELETE', `/conversations/${gid}`);
    ok('new owner dissolves group', dis.status === 204 || dis.status === 200, `got ${dis.status}`);
  }

  // --- 删好友 → DM 只读 ---
  await api(B.access_token, 'DELETE', `/friends/${A.user.id}`);
  wsa.send({ type: 'send_message', conversation_id: A.dmId, client_msg_id: 'x', content: 'still?' });
  {
    try {
      await wsa.waitType('error', () => true, 5000);
      ok('send after unfriend rejected', true);
    } catch { ok('send after unfriend rejected', false, 'no error frame'); }
  }
  wsa.close(); wsb2.close();

  // --- 管理员后台 ---
  {
    const r = await api(A.access_token, 'GET', '/admin/overview');
    ok('non-admin gets 403 on admin api', r.status === 403);
    const r2 = await api(A.access_token, 'GET', '/admin/users');
    ok('non-admin gets 403 on admin users', r2.status === 403);
  }
  // 完整 admin 流程需要管理员凭据（环境变量门控，库里首个注册者即管理员）
  if (process.env.E2E_ADMIN_USER && process.env.E2E_ADMIN_PASS) {
    const la = await api(null, 'POST', '/auth/login', {
      username: process.env.E2E_ADMIN_USER,
      password: process.env.E2E_ADMIN_PASS,
    });
    ok('admin login', la.status === 200 && la.json.user.role === 'admin', `got ${la.status}`);
    const at = la.json.access_token;
    const ov = await api(at, 'GET', '/admin/overview');
    ok('admin overview', ov.status === 200 && ov.json.user_count >= 3, JSON.stringify(ov.json).slice(0, 120));
    const ul = await api(at, 'GET', `/admin/users?q=${B.username}`);
    ok('admin user search', ul.status === 200 && ul.json.length === 1);
    // 禁用 B：B 在线时收 force_logout → REST/登录全拒
    const wsb3 = new Ws();
    await wsb3.connect(B.access_token);
    const dis = await api(at, 'POST', `/admin/users/${B.user.id}/disable`);
    ok('disable user', dis.status === 204);
    try {
      const f = await wsb3.waitType('force_logout', () => true, 5000);
      ok('kicked via force_logout frame', f.reason === 'disabled');
    } catch { ok('kicked via force_logout frame', false, 'no frame'); }
    const dm = await api(B.access_token, 'GET', '/users/me');
    ok('disabled user rejected on REST', dm.status === 403);
    const dl = await api(null, 'POST', '/auth/login', { username: B.username, password: B.password });
    ok('disabled user cannot login', dl.status === 403);
    const en = await api(at, 'POST', `/admin/users/${B.user.id}/enable`);
    ok('enable user', en.status === 204);
    // 禁用时 session_version 已 +1：旧 token 永久失效，启用后须重新登录
    const stale = await api(B.access_token, 'GET', '/users/me');
    ok('old token stays revoked after enable', stale.status === 401);
    const relog = await api(null, 'POST', '/auth/login', { username: B.username, password: B.password });
    ok('enabled user can re-login', relog.status === 200);
    wsb3.close();
  } else {
    console.log('  (skip admin-flow asserts: set E2E_ADMIN_USER/E2E_ADMIN_PASS to enable)');
  }

  const v2 = await runV2(A, C);
  const bot = await runBot(A);
  writeFileSync(STATE, JSON.stringify({ base: BASE, A, B, C, dmId: A.dmId, attachments, v2, bot }, null, 2));
  console.log(`state saved -> ${STATE}`);
}

async function phase2() {
  const s = JSON.parse(readFileSync(STATE, 'utf8'));
  if (s.base !== BASE) throw new Error('phase2 must use the same deployment as phase1');
  const login = await api(null, 'POST', '/auth/login', { username: s.A.username, password: s.A.password });
  ok('alice still exists after restart', login.status === 200);
  const t = login.json.access_token;
  {
    const msgs = await api(t, 'GET', `/conversations/${s.dmId}/messages`);
    const contents = (msgs.json.messages || []).map((m) => m.content);
    ok('message history persisted', contents.includes('hi bob') && contents.includes('off2'), JSON.stringify(contents));
    const convs = await api(t, 'GET', '/conversations');
    const dm = convs.json.find((c) => c.id === s.dmId);
    // phase1 共 5 条 DM：hi bob(1) off1(2) off2(3) 文件(4) 图片(5) —— HEAD 旧断言 3 早于附件消息加入，属过时断言
    ok('conversation metadata persisted', dm && dm.last_seq === 5, `last_seq=${dm?.last_seq}`);
    ok('attachment baseline recorded', s.attachments.length === 2);
    for (const attachment of s.attachments) {
      const message = msgs.json.messages.find((m) => m.id === attachment.message_id);
      ok('attachment metadata persisted', message?.attachment?.id === attachment.attachment_id);
      const download = await fetch(`${BASE}/files/${attachment.attachment_id}`);
      const bytes = Buffer.from(await download.arrayBuffer());
      ok('attachment bytes persisted', download.status === 200 && bytes.length === attachment.size &&
        createHash('sha256').update(bytes).digest('hex') === attachment.sha256);
    }
  }
  const lb = await api(null, 'POST', '/auth/login', { username: s.B.username, password: s.B.password });
  const fr = await api(lb.json.access_token, 'GET', '/friends');
  ok('bob still registered', lb.status === 200);
  console.log(`  (bob friends now: ${(fr.json || []).map((f) => f.user.username).join(',') || 'none — unfriended in phase1'})`);

  console.log('== V2 重启后持久化 ==');
  const v = s.v2 || {};
  {
    const av = await fetch(`${BASE}/users/${v.avatarUserId}/avatar?v=${v.avatarVersion}`);
    ok('avatar persisted after restart', av.status === 200, `got ${av.status}`);
    const convs = await api(t, 'GET', '/conversations');
    const self = (convs.json || []).find((c) => c.id === v.selfConvId);
    ok('self conversation persisted', self?.kind === 'self', `kind=${self?.kind}`);
    const grp = (convs.json || []).find((c) => c.id === v.announceConvId);
    ok('announcement persisted', grp?.announcement?.text === v.announceText, JSON.stringify(grp?.announcement));
    const pins = await api(t, 'GET', `/conversations/${v.pinConvId}/pins`);
    const pinned = Array.isArray(pins.json) && pins.json.some((p) => p.message?.id === v.pinMessageId);
    ok('pin persisted', pins.status === 200 && pinned, `got ${pins.status} ${JSON.stringify(pins.json)?.slice(0, 160)}`);
  }

  console.log('== bot 重启后持久化 ==');
  const b = s.bot || {};
  {
    const bots = await api(t, 'GET', '/bots');
    const bot = (bots.json || []).find((x) => x.id === b.id);
    ok('bot persisted', bots.status === 200 && bot?.username === b.username, `got ${bots.status}`);
    const msgs = await api(t, 'GET', `/conversations/${b.dmId}/messages`);
    const hit = (msgs.json?.messages || []).some((m) => m.sender_id === b.id && m.content === b.text);
    ok('bot message persisted', hit);
    const again = await api(b.token, 'POST', '/hooks', { content: 'after restart' });
    ok('bot hook works after restart', again.status === 200, `got ${again.status}`);
  }

  console.log('== MF1 合并转发（聊天记录）==');
  {
    // 一对新用户：m1 合并转发 m2 的两条私聊消息回同一会话
    const M1 = mk('m1'), M2 = mk('m2'), M3 = mk('m3');
    for (const u of [M1, M2, M3]) {
      const r = await api(null, 'POST', '/auth/register', { username: u.username, password: u.password });
      Object.assign(u, r.json);
    }
    const freq = await api(M1.access_token, 'POST', '/friends/requests', { username: M2.username, message: 'mf1' });
    ok('mf1 friend request', freq.status === 200 || freq.status === 201, `got ${freq.status}`);
    const inbox = await api(M2.access_token, 'GET', '/friends/requests');
    const fr = (inbox.json?.incoming || []).find((x) => x.user?.username === M1.username);
    const acc = await api(M2.access_token, 'POST', `/friends/requests/${fr?.id}/accept`);
    ok('mf1 friend accepted', acc.status === 200 || acc.status === 204, `got ${acc.status}`);
    const m1convs = await api(M1.access_token, 'GET', '/conversations');
    const dm = (m1convs.json || []).find((c) => c.kind === 'dm' && c.peer?.username === M2.username);
    ok('mf1 dm exists', !!dm?.id);

    const w = new Ws();
    await w.connect(M2.access_token);
    await w.waitType('presence_init');
    const texts = [`mf1-one-${rid}`, `mf1-two-${rid}`];
    const ids = [];
    for (const [i, text] of texts.entries()) {
      w.send({ type: 'send_message', conversation_id: dm.id, client_msg_id: `mf1s${i}`, content: text });
      const ack = await w.waitType('message_ack', (m) => m.client_msg_id === `mf1s${i}`);
      ids.push(ack.message?.id);
    }

    const fb = await api(M1.access_token, 'POST', '/messages/forward-batch', {
      conversation_id: dm.id, message_ids: ids, targets: [dm.id],
      mode: 'merged', client_op_id: randomUUID(),
    });
    const t = fb.json?.results?.[0];
    ok('merged forward ok', fb.status === 200 && t?.ok === true && t.forwarded === 1 &&
      t.message_ids?.length === 1 && Object.keys(t.skipped || {}).length === 0,
      `got ${fb.status} ${JSON.stringify(fb.json)?.slice(0, 200)}`);
    const rid2 = t?.message_ids?.[0];

    // 接收方视角：WS 帧与历史都只带 record_preview，不带完整快照
    const frame = await w.waitType('message', (m) => m.message?.kind === 'chat_record');
    ok('ws frame is chat_record with preview', frame.message?.id === rid2 &&
      frame.message?.record_preview?.count === 2 &&
      frame.message?.record === undefined, JSON.stringify(frame.message?.record_preview));
    const hist = await api(M2.access_token, 'GET', `/conversations/${dm.id}/messages`);
    const rec = (hist.json?.messages || []).find((m) => m.id === rid2);
    ok('recipient sees record_preview', rec?.kind === 'chat_record' &&
      rec.record_preview?.title === `${M1.user.display_name} 和 ${M2.user.display_name} 的聊天记录` &&
      rec.record_preview?.items?.length === 2 && rec.record === undefined,
      JSON.stringify({ title: rec?.record_preview?.title, n: rec?.record_preview?.items?.length }));
    ok('fallback content first line', (rec?.content || '').startsWith('[聊天记录] '),
      `got ${(rec?.content || '').slice(0, 40)}`);

    // GET record：成员拿全量，非成员同形 403
    const full = await api(M1.access_token, 'GET', `/messages/${rid2}/record`);
    ok('member GET record 200', full.status === 200 && full.json?.v === 1 &&
      full.json?.count === 2 && full.json?.items?.[0]?.content === texts[0] &&
      full.json?.items?.[0]?.sender_name === M2.user.display_name,
      `got ${full.status} ${JSON.stringify(full.json)?.slice(0, 200)}`);
    const denied = await api(M3.access_token, 'GET', `/messages/${rid2}/record`);
    ok('non-member GET record 403', denied.status === 403, `got ${denied.status}`);
    w.close();
  }
}

// ---------- V2 ----------

async function selfConvs(token) {
  const r = await api(token, 'GET', '/conversations');
  const list = r.json || [];
  return { status: r.status, list, selves: list.filter((c) => c.kind === 'self') };
}

async function runV2(A, C) {
  const v2 = {};

  console.log('== V2 注册开关 / 邀请码 ==');
  if (!process.env.E2E_ADMIN_USER || !process.env.E2E_ADMIN_PASS) {
    console.log('  (skip: set E2E_ADMIN_USER/E2E_ADMIN_PASS)');
  } else {
    const la = await api(null, 'POST', '/auth/login', {
      username: process.env.E2E_ADMIN_USER,
      password: process.env.E2E_ADMIN_PASS,
    });
    ok('admin login for registration settings', la.status === 200 && la.json?.user?.role === 'admin', `got ${la.status}`);
    if (la.status === 200) {
      const at = la.json.access_token;
      const cur = await api(at, 'GET', '/admin/settings');
      const prev = cur.json?.registration_mode;
      ok('read current registration mode', cur.status === 200 && typeof prev === 'string', `got ${cur.status}`);
      if (typeof prev === 'string') {
        try {
          const set = await api(at, 'PUT', '/admin/settings', { registration_mode: 'invite' });
          ok('switch registration to invite', set.status === 200 && set.json?.registration_mode === 'invite', `got ${set.status}`);
          const bare = mk('n');
          const noCode = await api(null, 'POST', '/auth/register', { username: bare.username, password: bare.password });
          ok('register without invite 403', noCode.status === 403, `got ${noCode.status}`);
          const inv = await api(at, 'POST', '/admin/invites', { max_uses: 1 });
          ok('create one-time invite', inv.status === 200 && !!inv.json?.code, `got ${inv.status}`);
          const invited = mk('i');
          const yes = await api(null, 'POST', '/auth/register', {
            username: invited.username, password: invited.password, invite_code: inv.json?.code,
          });
          ok('register with invite', yes.status === 200 && !!yes.json?.access_token, `got ${yes.status}`);
          const reuse = mk('j');
          const again = await api(null, 'POST', '/auth/register', {
            username: reuse.username, password: reuse.password, invite_code: inv.json?.code,
          });
          ok('reused invite 403', again.status === 403, `got ${again.status}`);
        } finally {
          const back = await api(at, 'PUT', '/admin/settings', { registration_mode: prev });
          const check = await api(at, 'GET', '/admin/settings');
          ok('registration mode restored', back.status === 200 && check.json?.registration_mode === prev,
            `put ${back.status} now=${check.json?.registration_mode} want=${prev}`);
          if (check.json?.registration_mode !== prev) {
            throw new Error(`failed to restore registration_mode to ${prev}`);
          }
        }
      }
    }
  }

  console.log('== V2 登录设备 / 改密 ==');
  {
    // 注册也会建一条 session。先登出它，再登录两次，列表才恰好是这两台设备。
    const S = mk('s');
    const reg = await api(null, 'POST', '/auth/register', {
      username: S.username, password: S.password, device_label: '注册设备',
    });
    ok('session user registered', reg.status === 200, `got ${reg.status}`);
    const out = await api(null, 'POST', '/auth/logout', { refresh_token: reg.json?.refresh_token });
    ok('logout register session', out.status === 204, `got ${out.status}`);
    const d1 = await api(null, 'POST', '/auth/login', {
      username: S.username, password: S.password, device_label: '设备甲',
    });
    const d2 = await api(null, 'POST', '/auth/login', {
      username: S.username, password: S.password, device_label: '设备乙',
    });
    ok('two device logins', d1.status === 200 && d2.status === 200, `got ${d1.status}/${d2.status}`);
    const list = await api(d1.json.access_token, 'GET', '/users/me/sessions');
    const rows = list.json || [];
    ok('two logins → two sessions', list.status === 200 && rows.length === 2, `got ${list.status} n=${rows.length}`);
    const other = rows.find((s) => s.device_label === '设备乙');
    const rev = await api(d1.json.access_token, 'DELETE', `/users/me/sessions/${other?.id}`);
    ok('revoke other session', rev.status === 204, `got ${rev.status}`);
    const dead = await api(d2.json.access_token, 'GET', '/users/me');
    ok('revoked device token 401', dead.status === 401, `got ${dead.status}`);
    const d3 = await api(null, 'POST', '/auth/login', {
      username: S.username, password: S.password, device_label: '设备丙',
    });
    ok('third device login', d3.status === 200, `got ${d3.status}`);
    const nextPw = 'pass654321';
    const ch = await api(d1.json.access_token, 'POST', '/users/me/password', {
      old_password: S.password, new_password: nextPw,
    });
    ok('change password', ch.status === 200 && !!ch.json?.access_token, `got ${ch.status}`);
    const otherDead = await api(d3.json.access_token, 'GET', '/users/me');
    ok('other device token 401 after password change', otherDead.status === 401, `got ${otherDead.status}`);
    const curOk = await api(ch.json?.access_token, 'GET', '/users/me');
    ok('current device new token works', curOk.status === 200, `got ${curOk.status}`);
    const nl = await api(null, 'POST', '/auth/login', { username: S.username, password: nextPw });
    ok('new password can login', nl.status === 200, `got ${nl.status}`);
    const old = await api(null, 'POST', '/auth/login', { username: S.username, password: S.password });
    ok('old password rejected', old.status === 401, `got ${old.status}`);
  }

  console.log('== V2 头像 ==');
  {
    const before = A.user?.avatar_version ?? 0;
    const up = await uploadMultipart(A.access_token, 'PUT', '/users/me/avatar', {}, {
      bytes: TINY_PNG, type: 'image/png', name: 'a.png',
    });
    ok('upload avatar', up.status === 200 && up.json?.avatar_version === before + 1 && up.json?.has_avatar === true,
      `got ${up.status} v=${up.json?.avatar_version}`);
    const version = up.json?.avatar_version;
    const got = await fetch(`${BASE}/users/${A.user.id}/avatar?v=${version}`);
    ok('get avatar 200', got.status === 200, `got ${got.status}`);
    const spoof = await uploadMultipart(A.access_token, 'PUT', '/users/me/avatar', {}, {
      bytes: Buffer.from('<html><script>alert(1)</script></html>'), type: 'image/png', name: 'a.png',
    });
    ok('spoofed avatar type 400', spoof.status === 400, `got ${spoof.status}`);
    v2.avatarUserId = A.user.id;
    v2.avatarVersion = version;
  }

  const mine = await selfConvs(A.access_token);
  const selfId = mine.selves[0]?.id;
  ok('alice has one self conversation', mine.selves.length === 1 && !!selfId, `n=${mine.selves.length}`);
  v2.selfConvId = selfId;

  console.log('== V2 缩略图 ==');
  {
    const big = pngRgb(481, 2);
    const up = await uploadMultipart(A.access_token, 'POST', '/files', { conversation_id: selfId }, {
      bytes: big, type: 'image/png', name: 'big.png',
    });
    ok('large png has_thumb', up.status === 200 && up.json?.has_thumb === true,
      `got ${up.status} ${JSON.stringify(up.json)?.slice(0, 160)}`);
    const thumb = await fetch(`${BASE}/files/${up.json?.id}?thumb=1`);
    const ctype = thumb.headers.get('content-type') || '';
    ok('thumb is image/jpeg', thumb.status === 200 && ctype.startsWith('image/jpeg'),
      `got ${thumb.status} ${ctype}`);
  }

  console.log('== V2 语音 ==');
  {
    const up = await uploadMultipart(A.access_token, 'POST', '/files', {
      conversation_id: selfId, duration_ms: '3200',
    }, { bytes: Buffer.from('webm-bytes'), type: 'audio/webm', name: 'voice.webm' });
    ok('upload voice', up.status === 200 && up.json?.duration_ms === 3200, `got ${up.status} ${JSON.stringify(up.json)?.slice(0, 160)}`);
    const w = new Ws();
    await w.connect(A.access_token);
    await w.waitType('presence_init');
    w.send({
      type: 'send_message', conversation_id: selfId, client_msg_id: 'v2voice',
      content: '', attachment_id: up.json?.id,
    });
    const ack = await w.waitType('message_ack', (m) => m.client_msg_id === 'v2voice');
    ok('voice message kind=audio', ack.message?.kind === 'audio' && ack.message?.content === '[语音]',
      JSON.stringify({ kind: ack.message?.kind, content: ack.message?.content }));
    w.close();
  }

  console.log('== V2 文件传输助手 / 转发 ==');
  {
    const F = mk('f');
    const reg = await api(null, 'POST', '/auth/register', { username: F.username, password: F.password });
    ok('forward user registered', reg.status === 200, `got ${reg.status}`);
    Object.assign(F, reg.json);
    const convs = await selfConvs(F.access_token);
    ok('new user has exactly one self conversation',
      convs.status === 200 && convs.selves.length === 1 && convs.list.length === 1,
      `status=${convs.status} self=${convs.selves.length} all=${convs.list.length}`);
    const fid = convs.selves[0].id;
    const w = new Ws();
    await w.connect(F.access_token);
    await w.waitType('presence_init');
    const body = `fwd-${rid}`;
    w.send({ type: 'send_message', conversation_id: fid, client_msg_id: 'v2fwd', content: body });
    const ack = await w.waitType('message_ack', (m) => m.client_msg_id === 'v2fwd');
    const src = ack.message?.id;
    const op = randomUUID();
    const fwd = await api(F.access_token, 'POST', `/messages/${src}/forward`, {
      conversation_ids: [fid], client_op_id: op,
    });
    const item = fwd.json?.results?.[0];
    ok('forward to self', fwd.status === 200 && item?.ok === true,
      `got ${fwd.status} ${JSON.stringify(fwd.json)?.slice(0, 200)}`);
    ok('forwarded_from is sender display name', item?.message?.forwarded_from === F.user.display_name,
      `got ${item?.message?.forwarded_from}`);
    const mid = item?.message?.id;
    const retry = await api(F.access_token, 'POST', `/messages/${src}/forward`, {
      conversation_ids: [fid], client_op_id: op,
    });
    ok('same client_op_id does not duplicate',
      retry.status === 200 && retry.json?.results?.[0]?.message?.id === mid,
      `got ${retry.status} id=${retry.json?.results?.[0]?.message?.id}`);
    const hist = await api(F.access_token, 'GET', `/conversations/${fid}/messages`);
    const copies = (hist.json?.messages || []).filter((m) => m.forwarded_from);
    ok('history has one forwarded copy', copies.length === 1 && copies[0].id === mid, `n=${copies.length}`);
    w.close();
  }

  console.log('== V2 全局搜索 ==');
  let gid = null;
  {
    const req = await api(A.access_token, 'POST', '/friends/requests', { username: C.username, message: 'v2' });
    ok('friend request A->C', req.status === 200 || req.status === 201, `got ${req.status}`);
    const incoming = await api(C.access_token, 'GET', '/friends/requests');
    const hit = (incoming.json?.incoming || []).find((x) => x.user?.username === A.username);
    const acc = await api(C.access_token, 'POST', `/friends/requests/${hit?.id}/accept`);
    ok('C accepts A', acc.status === 200 || acc.status === 204, `got ${acc.status}`);
    const convs = await api(A.access_token, 'GET', '/conversations');
    const dm = (convs.json || []).find((c) => c.kind === 'dm' && c.peer?.username === C.username);
    ok('DM A-C exists', !!dm?.id);
    const g = await api(A.access_token, 'POST', '/conversations/group', { name: `v2群${rid}`, member_ids: [C.user.id] });
    ok('create v2 group', g.status === 200 || g.status === 201, `got ${g.status}`);
    gid = g.json?.id;
    const kw = `zq${rid}kw`;
    const w = new Ws();
    await w.connect(A.access_token);
    await w.waitType('presence_init');
    w.send({ type: 'send_message', conversation_id: gid, client_msg_id: 'v2pin', content: `pin-${rid}` });
    const pinAck = await w.waitType('message_ack', (m) => m.client_msg_id === 'v2pin');
    w.send({ type: 'send_message', conversation_id: dm.id, client_msg_id: 'v2s1', content: kw });
    await w.waitType('message_ack', (m) => m.client_msg_id === 'v2s1');
    w.send({ type: 'send_message', conversation_id: gid, client_msg_id: 'v2s2', content: kw });
    const m2 = await w.waitType('message_ack', (m) => m.client_msg_id === 'v2s2');
    const q = encodeURIComponent(kw);
    const found = await api(A.access_token, 'GET', `/search?q=${q}`);
    const hits = (found.json?.messages || []).filter((m) => (m.content || '').includes(kw));
    ok('global search returns two hits', found.status === 200 && hits.length === 2,
      `got ${found.status} n=${hits.length}`);
    const del = await api(A.access_token, 'DELETE', `/messages/${m2.message?.id}`);
    ok('delete one search hit', del.status === 200, `got ${del.status}`);
    const found2 = await api(A.access_token, 'GET', `/search?q=${q}`);
    const hits2 = (found2.json?.messages || []).filter((m) => (m.content || '').includes(kw));
    ok('global search returns one hit after delete', found2.status === 200 && hits2.length === 1,
      `got ${found2.status} n=${hits2.length}`);
    w.close();
    v2.pinMessageId = pinAck.message?.id;
  }

  console.log('== V2 公告 / 置顶 ==');
  {
    const text = `公告${rid}`;
    const ann = await api(A.access_token, 'PUT', `/conversations/${gid}/announcement`, { text });
    ok('owner sets announcement', ann.status === 200 && ann.json?.text === text, `got ${ann.status}`);
    const listed = await api(C.access_token, 'GET', '/conversations');
    const grp = (listed.json || []).find((c) => c.id === gid);
    ok('member list has announcement', grp?.announcement?.text === text, JSON.stringify(grp?.announcement));
    const pin = await api(C.access_token, 'PUT', `/conversations/${gid}/pins/${v2.pinMessageId}`);
    ok('member pins a message', pin.status === 204, `got ${pin.status}`);
    const pins = await api(C.access_token, 'GET', `/conversations/${gid}/pins`);
    const hit = Array.isArray(pins.json) && pins.json.some((p) => p.message?.id === v2.pinMessageId);
    ok('pins list returns it', pins.status === 200 && hit, `got ${pins.status} ${JSON.stringify(pins.json)?.slice(0, 180)}`);
    v2.announceConvId = gid;
    v2.announceText = text;
    v2.pinConvId = gid;
  }

  console.log('== V2 会议地址 ==');
  {
    const info = await api(null, 'GET', '/server-info');
    ok('server-info has meeting_base_url', info.status === 200 && typeof info.json?.meeting_base_url === 'string',
      `got ${info.status} ${JSON.stringify(info.json)?.slice(0, 160)}`);
  }

  console.log('== V2 推送 ==');
  {
    const push = await api(A.access_token, 'GET', '/users/me/push');
    ok('push settings expose available', push.status === 200 && typeof push.json?.available === 'boolean',
      `got ${push.status} ${JSON.stringify(push.json)?.slice(0, 160)}`);
  }

  return v2;
}

// ---------- webhook bot ----------
// 只测入站：出站回调要一个 bot 服务器可达的地址，集成测试里覆盖。

async function runBot(A) {
  console.log('== bot ==');
  const relog = await api(null, 'POST', '/auth/login', { username: A.username, password: A.password });
  const ta = relog.json?.access_token;
  const username = `e2ebot${rid}`.slice(0, 20);
  const created = await api(ta, 'POST', '/bots', { username });
  ok('create bot', created.status === 200 && !!created.json?.secret, `got ${created.status}`);
  const bot = created.json || {};
  const added = await api(ta, 'POST', `/bots/${bot.id}/tokens`, { name: 'e2e' });
  const token = added.json?.tokens?.[0]?.token;
  ok('add bot token', added.status === 200 && !!token, `got ${added.status}`);
  const convs = await api(ta, 'GET', '/conversations');
  const dm = (convs.json || []).find((c) => c.kind === 'dm' && c.peer?.id === bot.id);
  ok('bot DM listed', dm?.peer?.is_bot === true, `peer=${JSON.stringify(dm?.peer)}`);
  const text = `deploy ${rid}`;
  const hook = await api(token, 'POST', '/hooks', { content: text });
  ok('hook posts as bot', hook.status === 200, `got ${hook.status}`);
  const bad = await api('not-a-token', 'POST', '/hooks', { content: 'x' });
  ok('unknown hook token 401', bad.status === 401, `got ${bad.status}`);
  return { id: bot.id, username, dmId: dm?.id, text, token };
}

(PHASE === 'phase2' ? phase2() : phase1())
  .then(() => { console.log(`\n${pass} passed, ${fail} failed`); process.exit(fail ? 1 : 0); })
  .catch((e) => { console.error('FATAL', e); process.exit(1); });
