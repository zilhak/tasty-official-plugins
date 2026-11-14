import type { Register } from 'claude-code';

// A plugin's own hooks do not see the prompts its timers submit, so this gate is
// a separate plugin. It runs where the engine dispatches the prompt and survives
// tasty-notify reloads: a queued copy from a replaced module and the fresh
// module's resubmission of the same delivery enter once between them.
// Self-contained: tests load this function as an inline plugin, without the file.
export const register: Register = (on) => {
  // tasty-notify writes this first line; keep the format in step with its delivery().
  const marker = /^\[\[tasty-notify (\d+)\]\]\n/;
  // Shown to the user as the reason when a second copy is dropped.
  const duplicates: Record<string, string> = {
    en: 'Tasty notification already delivered; duplicate skipped',
    ko: 'Tasty 알림이 이미 전달되어 중복을 건너뜁니다',
    ja: 'Tasty 通知は配信済みのため重複をスキップしました',
    'zh-Hans': 'Tasty 通知已送达；已跳过重复通知',
  };
  // Names this module copy in the record. An entry still running here is in
  // `running`; one recorded by another copy lost its dispatch with that copy.
  const owner = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  const running = new Set<number>();
  on('prompt.submit', async ($, e, next) => {
    const found = e.origin.kind === 'plugin' && e.origin.name === 'tasty-notify'
      ? marker.exec(e.text) : null;
    const seq = Number(found?.[1]);
    if (!found || !Number.isSafeInteger(seq) || seq < 1) return next(e);
    const drop = async () => ({ drop: duplicates[(await $.env.get('TASTY_LOCALE')) ?? ''] ?? duplicates.en! });
    const held = await $.state.get({ plugin: 'tasty-notify-gate', key: 'record' });
    const record = held.value ?? { delivered: 0, inflight: 0, owner: '' };
    // Every drop is loss-free: tasty-notify keeps the delivery pending until
    // `delivered` covers it, and resubmits it on a later poll.
    if (seq <= record.delivered) return drop();
    if (record.owner === owner && running.has(record.inflight) && seq <= record.inflight) return drop();
    // A stale in-flight entry from a lost copy is taken over: it may have entered,
    // but treating it as entered could lose it, so a repeat is the accepted risk.
    const claim = await $.state.set({ plugin: 'tasty-notify-gate', key: 'record' },
      { delivered: record.delivered, inflight: seq, owner }, { ifVersion: held.version });
    if (!claim.isSet) return drop();
    running.add(seq);
    let result;
    try {
      result = await next({ ...e, text: e.text.slice(found[0].length) });
    } catch (error) {
      running.delete(seq);
      await $.state.set({ plugin: 'tasty-notify-gate', key: 'record' },
        { delivered: record.delivered, inflight: 0, owner: '' }, { ifVersion: claim.version });
      throw error;
    }
    running.delete(seq);
    if (result.drop !== undefined) {
      // Refused beneath: release the claim; the pending delivery is submitted again.
      await $.state.set({ plugin: 'tasty-notify-gate', key: 'record' },
        { delivered: record.delivered, inflight: 0, owner: '' }, { ifVersion: claim.version });
      return result;
    }
    // Only now may tasty-notify clear its pending delivery.
    const confirmed = { delivered: Math.max(record.delivered, seq), inflight: 0, owner: '' };
    const done = await $.state.set({ plugin: 'tasty-notify-gate', key: 'record' }, confirmed,
      { ifVersion: claim.version });
    if (!done.isSet) await $.state.set({ plugin: 'tasty-notify-gate', key: 'record' }, confirmed);
    return result;
  });
};
