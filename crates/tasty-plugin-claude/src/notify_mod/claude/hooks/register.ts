import type { Register, Timer } from 'claude-code';
import type { NotifyCursor, NotifyPending } from '../types/index';
import { batch, MAX_READ, position, retention } from './reader';
import { delivery } from './messages';

const cursorRef = { plugin: 'tasty-notify', key: 'cursor' } as const;
const gateRef = { plugin: 'tasty-notify-gate', key: 'record' } as const;

export const register: Register = (on) => {
  let timer: Timer | undefined;
  let stopped = false;
  let generation = 0;
  let activeNamespace: string | undefined;
  on('session.start', async ($, e, next) => {

    const surface = await $.env.get('TASTY_SURFACE_ID');
    const home = await $.env.get('TASTY_PARENT_HOME');
    const locale = await $.env.get('TASTY_LOCALE');
    if (!surface || !/^\d+$/.test(surface) || !home || !/^(\/|[A-Za-z]:[\\/])/.test(home)) {
      timer?.cancel();
      stopped = true;
      generation++;
      activeNamespace = undefined;
      await $.env.set('TASTY_CLAUDE_NOTIFY_MOD', undefined);
      return next(e);
    }
    const path = `${home.replace(/[\\/]$/, '')}/notify/${surface}.log`;
    // A repeated start in this registration must not retire an accepted submit
    // still waiting to checkpoint. A new VM on real reload has no such owner.
    if (!stopped && activeNamespace === path) return next(e);
    timer?.cancel();
    stopped = false;
    activeNamespace = undefined;
    const run = ++generation;
    const currentRun = () => !stopped && generation === run;
    await $.env.set('TASTY_CLAUDE_NOTIFY_MOD', undefined);
    const namespace = path;
    let consumed: { bytes: Uint8Array; base: number; mtime: number } | undefined;
    let cursor = (await $.state.get(cursorRef)).value;
    const save = async (value: NotifyCursor) => {
      if (!currentRun()) throw new Error('notify watcher retired');
      if (cursor && cursor.namespace === value.namespace && cursor.next === value.next
          && cursor.retention === value.retention && cursor.missing === value.missing
          && cursor.seq === value.seq && cursor.pending?.seq === value.pending?.seq) return;
      const result = await $.state.set(cursorRef, value);
      if (!result.isSet) throw new Error('notify cursor refused');
      cursor = value;
    };
    const snapshot = async (): Promise<{ bytes: Uint8Array; base: number; mtime: number } | undefined | null> => {
      // A missing meta file is the old writer's zero baseline. Other I/O errors retry.
      const meta = async () => await $.fs.exists(`${path}.meta`)
        ? retention(await $.fs.read(`${path}.meta`)) : 0;
      const base = await meta();
      if (!await $.fs.exists(path)) return undefined;
      const stat = await $.fs.stat(path);
      if (stat.kind !== 'file' || stat.isLink || stat.size > MAX_READ) throw new Error('notify log bounds');
      if (consumed && !cursor?.missing && cursor?.next === base + stat.size
          && consumed.base === base && consumed.bytes.length === stat.size
          && consumed.mtime === stat.mtimeMs) return null;
      const raw = await $.fs.read(path, { as: 'bytes' });
      const bytes = Uint8Array.from(atob(raw.base64), c => c.charCodeAt(0));
      if (bytes.length > MAX_READ || base !== await meta()) throw new Error('notify log changed');
      return { bytes, base, mtime: stat.mtimeMs };
    };
    try {
      if (!cursor || cursor.namespace !== namespace) {
        const initial = await snapshot();
        if (initial) consumed = initial;
        // An unconfirmed delivery of a previous namespace stays pending and is still delivered.
        await save({ namespace, next: initial ? initial.base + initial.bytes.length : 0,
          retention: initial?.base ?? 0, missing: !initial, seq: cursor?.seq ?? 0,
          ...(cursor?.pending ? { pending: cursor.pending } : {}) });
      }
      const active = await $.env.set('TASTY_CLAUDE_NOTIFY_MOD', surface);
      if ('deny' in active) return next(e);
      activeNamespace = namespace;
    } catch {
      // Do not arm at a later EOF and silently skip children completed during startup.
      return next(e);
    }
    // A pending delivery is resubmitted with its exact text until the gate confirms
    // it entered, so a reload never widens, repeats or drops it. An in-flight claim
    // is not a confirmation: the entry can still be refused beneath the gate. A drop
    // is the gate's duplicate or a refusal; the next poll reads the gate again.
    const deliver = async (pending: NotifyPending) => {
      if (((await $.state.get(gateRef)).value?.delivered ?? 0) < pending.seq) {
        if (!currentRun()) return;
        const submitted = await $.prompt.submit({ text: pending.text });
        if (submitted.drop !== undefined) return;
      }
      await save(pending.namespace === namespace
        ? { namespace, next: pending.next, retention: pending.retention, missing: false, seq: pending.seq }
        : { namespace, next: cursor!.next, retention: cursor!.retention, missing: cursor!.missing, seq: pending.seq });
    };
    const poll = async () => {
      try {
        if (cursor!.pending) {
          consumed = undefined;
          await deliver(cursor!.pending);
          return;
        }
        const current = await snapshot();
        if (current === null) return;
        if (!current) {
          consumed = undefined;
          await save({ ...cursor!, next: 0, retention: 0, missing: true });
        } else {
          const replaced = consumed && consumed.base === current.base
            && consumed.bytes.length === current.bytes.length
            && consumed.bytes.some((byte, i) => byte !== current.bytes[i]);
          const start = replaced ? 0 : position(cursor!, current.base, current.bytes.length);
          const part = batch(current.bytes, start);
          if (!currentRun()) return;
          if (part.end > start) {
            // Above the gate's record too: a recreated cursor must not reuse an admitted seq.
            const gate = (await $.state.get(gateRef)).value;
            const seq = Math.max(cursor!.seq ?? 0, gate?.delivered ?? 0, gate?.inflight ?? 0) + 1;
            const pending = { namespace, seq, text: delivery(seq, locale, part.text),
              next: current.base + part.end, retention: current.base };
            // Durable before submit: a fresh module copy resubmits this exact text.
            await save({ ...cursor!, seq, pending });
            await deliver(pending);
          } else {
            await save({ namespace, next: current.base + part.end, retention: current.base,
              missing: false, seq: cursor!.seq ?? 0 });
          }
          consumed = part.end === current.bytes.length ? current : undefined;
        }
      } catch {
        // Keep the last accepted cursor on transient I/O or submit refusal; never PTY input.
      } finally {
        if (currentRun()) timer = $.clock.after(500, () => { void poll(); });
      }
    };
    timer = $.clock.after(500, () => { void poll(); });
    return next(e);
  });
  on('session.end', async ($, e, next) => {
    // /clear keeps the same process and watcher; it does not emit session.start.
    if (e.reason !== 'clear') {
      stopped = true;
      activeNamespace = undefined;
      generation++;
      timer?.cancel();
      await $.env.set('TASTY_CLAUDE_NOTIFY_MOD', undefined);
    }
    return next(e);
  });
};
