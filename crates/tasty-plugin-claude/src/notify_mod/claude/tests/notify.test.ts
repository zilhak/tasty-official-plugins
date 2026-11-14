import { test, expect, mock } from 'claude-code/testing';
import type { Engine } from 'claude-code/testing';
import type { On } from 'claude-code';
import { batch, position, retention } from '../hooks/reader';
import { delivery, heading } from '../hooks/messages';
import { register as gate } from '../gate/hooks/register';

// Installed beside this mod; most tests load both, as an install does.
const it = (name: string, body: ($: Engine, on: On) => unknown) =>
  test(name, { plugins: [{ name: 'tasty-notify-gate', register: gate }] }, body);

const root = '/owned/tasty/notify/7.log';
function fixture(on: On, variables: Record<string, string> = { TASTY_SURFACE_ID: '7', TASTY_PARENT_HOME: '/owned/tasty' }) {
  const clock = mock.clock(on);
  on('env.get', ($, e) => ({ value: variables[e.name] }));
  const files: Record<string, string> = { [root]: 'old\n', [`${root}.meta`]: 'retention_start=0                   \n' };
  const prompts: string[] = [];
  const origins: unknown[] = [];
  const env: Record<string, string | undefined> = {};
  let refuse = false;
  let readDelay = 0;
  let submitDelay = 0;
  let reject = false;
  let late: 'drop' | 'throw' | undefined;
  let mtime = 1;
  const reads = { body: 0, live: 0, max: 0, writes: 0 };
  on('state.set', ($, e, next) => { reads.writes++; return next(e); });
  on('env.set', ($, e) => { env[e.name] = e.value; return { value: {} }; });
  on('fs.exists', ($, e) => ({ value: e.path in files }));
  on('fs.stat', ($, e) => ({ value: { kind: 'file', isLink: false, size: new TextEncoder().encode(files[e.path]).length, mtimeMs: mtime } }));
  on('fs.read', async ($, e) => {
    if (e.path === root) {
      reads.body++; reads.live++; reads.max = Math.max(reads.max, reads.live);
      if (readDelay) await clock.sleep(readDelay);
      reads.live--;
    }
    return { value: e.as === 'bytes' ? { base64: btoa(Array.from(new TextEncoder().encode(files[e.path]), n => String.fromCharCode(n)).join('')) } : files[e.path]! };
  });
  on('prompt.submit', async ($, e) => { if (late) { const mode = late; late = undefined; await clock.sleep(submitDelay); if (mode === 'throw') throw new Error('late rejection'); return { drop: 'late refusal' }; } if (reject) { reject = false; throw new Error('test rejection'); } if (refuse) return { drop: 'test refusal' }; prompts.push(e.text); origins.push(e.origin); if (submitDelay) await clock.sleep(submitDelay); return { text: e.text, origin: e.origin }; });
  on('session.start', ($, e) => ({ cwd: e.cwd }));
  on('session.end', ($, e) => ({ sessionId: e.sessionId }));
  return { clock, files, prompts, origins, env, variables, reads, touch: () => { mtime++; }, submitDelay: (value: number) => { submitDelay = value; }, delay: (value: number) => { readDelay = value; }, refuse: (value: boolean) => { refuse = value; }, rejectOnce: () => { reject = true; }, lateOnce: (mode: 'drop' | 'throw') => { late = mode; } };
}
const start = ($: Engine) => $.session.start({ cwd: '/owned', surface: null, isInteractive: false });

it('startup ignores old log; fast completion is coalesced once; partial UTF8 line waits', async ($, on) => {
  const f = fixture(on);
  await start($);
  expect(f.env.TASTY_CLAUDE_NOTIFY_MOD).toBe('7');
  f.files[root] += 'child one\n두 번째\npartial';
  await f.clock.advance(500);
  expect(f.prompts.length).toBe(1);
  expect(f.prompts[0]).toContain('child one\n두 번째\n');
  expect(f.prompts[0]).toContain('not proof');
  expect(f.origins[0]).toEqual({ kind: 'plugin', name: 'tasty-notify' });
  await f.clock.advance(1000);
  expect(f.prompts.length).toBe(1);
  f.files[root] += '\n';
  await f.clock.advance(500);
  expect(f.prompts.length).toBe(2);
  expect(f.prompts[1]).toContain('partial\n');
});

it('reload resumes session cursor and has one timer; clear continues; end cancels', async ($, on) => {
  const f = fixture(on);
  await start($);
  f.files[root] += 'between loads\n';
  await start($);
  await f.clock.advance(500);
  expect(f.prompts.length).toBe(1);
  await $.session.end({ reason: 'clear', sessionId: 'test', resume: { id: 'test' } });
  f.files[root] += 'after clear\n';
  await f.clock.advance(500);
  expect(f.prompts.length).toBe(2);
  await $.session.end({ reason: 'prompt_input_exit', sessionId: 'test', resume: { id: 'test' } });
  f.files[root] += 'after end\n';
  await f.clock.advance(1000);
  expect(f.prompts.length).toBe(2);
});

it('truncation meta, uncounted shrink, delete and recreate resume at beginning', async ($, on) => {
  const f = fixture(on); await start($);
  f.files[`${root}.meta`] = 'retention_start=100                 \n';
  f.files[root] = 'new\n'; await f.clock.advance(500);
  expect(f.prompts[0]).toContain('new\n');
  f.files[root] = 'x\n'; await f.clock.advance(500);
  expect(f.prompts[1]).toContain('x\n');
  delete f.files[root]; await f.clock.advance(500);
  f.files[root] = 'recreated\n'; await f.clock.advance(500);
  expect(f.prompts[2]).toContain('recreated\n');
  f.files[`${root}.meta`] = 'retention_start=0                   \n';
  f.files[root] = 'host reboot\n'; await f.clock.advance(500);
  expect(f.prompts[3]).toContain('host reboot\n');
});

it('submit refusal retains cursor for retry; other surface logs are not read', async ($, on) => {
  const f = fixture(on); await start($);
  f.files['/owned/tasty/notify/8.log'] = 'foreign\n';
  f.files[root] += 'own\n'; f.refuse(true);
  await f.clock.advance(500); expect(f.prompts.length).toBe(0);
  f.refuse(false); await f.clock.advance(500);
  expect(f.prompts.length).toBe(1); expect(f.prompts[0]).toContain('own\n');
  await f.clock.advance(500); expect(f.prompts.length).toBe(1);
});

it('outside Tasty clears inherited marker and starts no watcher', async ($, on) => {
  const f = fixture(on, { TASTY_SURFACE_ID: '', TASTY_PARENT_HOME: '' }); await start($);
  await f.clock.advance(1000); expect(f.prompts.length).toBe(0);
  expect(f.env.TASTY_CLAUDE_NOTIFY_MOD).toBeUndefined();
});

test('reader byte offsets, bounded batches and invalid retention', () => {
  expect(retention('unknown=9\nretention_start=123   \n')).toBe(123);
  expect(retention('')).toBe(0);
  expect(() => retention('retention_start=999999999999999999999')).toThrow();
  expect(batch(new TextEncoder().encode('한\nrest'), 0).end).toBe(4);
  expect(batch(new TextEncoder().encode('partial'), 0).end).toBe(0);
  expect(position({ namespace: root, next: 40, retention: 20, missing: false }, 0, 100)).toBe(0);
});


it('initial missing log catches its first fast completion; large snapshots refuse without consuming', async ($, on) => {
  const f = fixture(on); delete f.files[root]; await start($);
  f.files[root] = 'first completion\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(1);
  const saved = f.files[root];
  f.files[root] += 'x'.repeat(1024 * 1024); await f.clock.advance(500);
  expect(f.prompts.length).toBe(1);
  f.files[root] = saved + 'after refusal\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(2);
});

it('new namespace takes a fresh EOF rather than replaying another surface', async ($, on) => {
  const f = fixture(on); await start($);
  f.variables.TASTY_PARENT_HOME = '/other';
  f.files['/other/notify/7.log'] = 'other old line\n';
  await start($); await f.clock.advance(500);
  expect(f.prompts.length).toBe(0);
  f.files['/other/notify/7.log'] += 'current only\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(1);
});


it('retirement during an asynchronous read cannot submit or rearm the watcher', async ($, on) => {
  const f = fixture(on); await start($);
  f.files[root] += 'retired completion\n'; f.delay(1000);
  await f.clock.advance(500);
  await $.session.end({ reason: 'prompt_input_exit', sessionId: 'test', resume: { id: 'test' } });
  await f.clock.advance(2000);
  expect(f.prompts.length).toBe(0);
});

test('visible heading has four translations and an English fallback', () => {
  expect(heading('en')).toContain('not proof');
  expect(heading('ko')).toContain('성공의 증거가 아닙니다');
  expect(heading('ja')).toContain('成功の証拠ではありません');
  expect(heading('zh-Hans')).toContain('不能证明任务成功');
  expect(heading('unknown')).toBe(heading('en'));
});


it('fully consumed unchanged snapshots suppress body reads but changed stat and meta are observed', async ($, on) => {
  const f = fixture(on); await start($);
  await f.clock.advance(1500); expect(f.reads.body).toBe(1); expect(f.reads.writes).toBe(1);
  f.touch(); await f.clock.advance(500); expect(f.reads.body).toBe(2);
  expect(f.prompts.length).toBe(0); expect(f.reads.writes).toBe(1);
  f.files[root] = 'new\n'; f.touch(); await f.clock.advance(500);
  expect(f.prompts.length).toBe(1); expect(f.prompts[0]).toContain('new\n');
  await f.clock.advance(1000); expect(f.reads.body).toBe(3);
  f.files[`${root}.meta`] = 'retention_start=4\n';
  await f.clock.advance(500); expect(f.reads.body).toBe(4);
  expect(f.prompts.length).toBe(2);
});

it('incomplete and refused snapshots are retried without overlapping body reads', async ($, on) => {
  const f = fixture(on); await start($);
  f.files[root] += 'partial'; await f.clock.advance(1000);
  expect(f.reads.body).toBe(3);
  f.files[root] += '\n'; f.refuse(true); await f.clock.advance(1000);
  // A refused delivery is retried from its pending text, not from a new read.
  expect(f.reads.body).toBe(4); expect(f.prompts.length).toBe(0);
  f.refuse(false); f.delay(1000); f.files[root] += 'next\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(1); expect(f.prompts[0]).not.toContain('next\n');
  await f.clock.advance(3000);
  expect(f.prompts.length).toBe(2); expect(f.reads.max).toBe(1);
  await f.clock.advance(1500); expect(f.reads.body).toBe(5);
});

it('repeated start while an accepted submit awaits its result preserves one checkpoint owner', async ($, on) => {
  const f = fixture(on); await start($);
  f.submitDelay(1500); f.files[root] += 'accepted\n';
  await f.clock.advance(500); expect(f.prompts.length).toBe(1);
  await start($); await f.clock.advance(2500);
  expect(f.prompts.length).toBe(1);
  f.submitDelay(0); f.files[root] += 'later\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(2); expect(f.prompts[1]).toContain('later\n');
});

const notifyOrigin = { kind: 'plugin', name: 'tasty-notify' } as const;
const end = ($: Engine) => $.session.end({ reason: 'prompt_input_exit', sessionId: 'test', resume: { id: 'test' } });

it('reload while a delivery is queued enters it once and keeps later lines', async ($, on) => {
  const f = fixture(on); await start($);
  f.submitDelay(1500); f.files[root] += 'queued\n';
  await f.clock.advance(500); expect(f.prompts.length).toBe(1);
  // A fresh module copy: the engine still holds the first copy's prompt.
  await end($); await start($);
  await f.clock.advance(2500);
  expect(f.prompts.length).toBe(1);
  f.submitDelay(0); f.files[root] += 'later\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(2); expect(f.prompts[1]).toContain('later\n');
  expect(f.prompts[1]).not.toContain('queued\n');
});

it('a repeated delivery entering after its first copy is dropped before the model', async ($, on) => {
  const f = fixture(on); await start($);
  f.files[root] += 'once\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(1);
  expect(f.prompts[0]).not.toContain('[[tasty-notify');
  const repeat = await $.prompt.submit({ text: `[[tasty-notify 1]]\n${heading(undefined)}\nonce\n`, wait: false, origin: notifyOrigin });
  expect(repeat.drop).toBeDefined();
  expect(f.prompts.length).toBe(1);
  const user = await $.prompt.submit({ text: '[[tasty-notify 1]]\nonce\n', wait: false, origin: { kind: 'composer' } });
  expect(user.drop).toBeUndefined();
  const other = await $.prompt.submit({ text: '[[tasty-notify 1]]\nonce\n', wait: false, origin: { kind: 'plugin', name: 'other' } });
  expect(other.drop).toBeUndefined();
  expect(f.prompts.length).toBe(3);
  expect(f.prompts[2]).toContain('[[tasty-notify 1]]');
});

it('a refused delivery is retried with its exact text before later lines', async ($, on) => {
  const f = fixture(on); await start($);
  f.refuse(true); f.files[root] += 'one\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(0);
  f.files[root] += 'two\n'; f.refuse(false); await f.clock.advance(500);
  expect(f.prompts.length).toBe(1);
  expect(f.prompts[0]).toContain('one\n'); expect(f.prompts[0]).not.toContain('two\n');
  await f.clock.advance(500);
  expect(f.prompts.length).toBe(2);
  expect(f.prompts[1]).toContain('two\n'); expect(f.prompts[1]).not.toContain('one\n');
  await end($); await start($); await f.clock.advance(1000);
  expect(f.prompts.length).toBe(2);
});

test('without the gate plugin deliveries still arrive once per poll', async ($, on) => {
  const f = fixture(on); await start($);
  f.files[root] += 'ungated\n'; await f.clock.advance(1500);
  expect(f.prompts.length).toBe(1); expect(f.prompts[0]).toContain('ungated\n');
});

it('the gate strips exactly the marker delivery() writes', async ($, on) => {
  const f = fixture(on); await start($);
  const entered = await $.prompt.submit({ text: delivery(7, 'ko', 'x\n'), wait: false, origin: notifyOrigin });
  expect(entered.drop).toBeUndefined();
  expect(f.prompts[0]).toBe(`${heading('ko')}\nx\n`);
});

it('a rejected entry is rolled back by the gate and delivered on retry', async ($, on) => {
  const f = fixture(on); await start($);
  f.rejectOnce(); f.files[root] += 'rejected once\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(0);
  await f.clock.advance(1000);
  expect(f.prompts.length).toBe(1); expect(f.prompts[0]).toContain('rejected once\n');
});

it('a new delivery takes a sequence above an existing gate record', async ($, on) => {
  const f = fixture(on); await start($);
  // The gate already admitted seq 5 while this cursor holds none, as a recreated cursor would.
  await $.prompt.submit({ text: delivery(5, undefined, 'earlier\n'), wait: false, origin: notifyOrigin });
  f.files[root] += 'next\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(2); expect(f.prompts[1]).toContain('next\n');
  await f.clock.advance(1000); expect(f.prompts.length).toBe(2);
});

it('a pending delivery survives a namespace change and is delivered once', async ($, on) => {
  const f = fixture(on); await start($);
  f.refuse(true); f.files[root] += 'old namespace\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(0);
  f.variables.TASTY_PARENT_HOME = '/other'; f.files['/other/notify/7.log'] = 'other old\n';
  await end($); f.refuse(false); await start($); await f.clock.advance(500);
  expect(f.prompts.length).toBe(1); expect(f.prompts[0]).toContain('old namespace\n');
  await f.clock.advance(1000); expect(f.prompts.length).toBe(1);
  f.files['/other/notify/7.log'] += 'other new\n'; await f.clock.advance(500);
  expect(f.prompts.length).toBe(2); expect(f.prompts[1]).toContain('other new\n');
  expect(f.prompts[1]).not.toContain('other old');
});

for (const mode of ['drop', 'throw'] as const) {
  it(`a reloaded watcher keeps a delivery refused (${mode}) after a slow lower hook`, async ($, on) => {
    const f = fixture(on); await start($);
    f.submitDelay(1500); f.lateOnce(mode); f.files[root] += 'held\n';
    await f.clock.advance(500); expect(f.prompts.length).toBe(0);
    // A fresh module copy polls while the gate's entry is still running beneath.
    await end($); await start($);
    await f.clock.advance(500); expect(f.prompts.length).toBe(0);
    await f.clock.advance(1500); f.submitDelay(0);
    await f.clock.advance(1000);
    expect(f.prompts.length).toBe(1); expect(f.prompts[0]).toContain('held\n');
    await f.clock.advance(1500); expect(f.prompts.length).toBe(1);
  });
}
