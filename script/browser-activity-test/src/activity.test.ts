import assert from 'node:assert/strict';
import { test } from 'node:test';
import { appendDiagnostic, classifyTimingGap, directReasons, emptyModel, isActive, MAX_EVENTS,
  observe, restoreModel, sessionStorageKey } from './activity.ts';
import type { Sample } from './activity.ts';
import { defaultSettings, sanitizeSettings, validFramePayload } from './probes.ts';

const visible: Sample = { visibility: 'visible', hidden: false, focused: true, windowBlurred: false, lifecycle: 'running' };

test('visible and focused is green; hidden, unfocused, frozen and pagehide are red', () => {
  assert.equal(isActive(visible), true);
  for (const change of [{ hidden: true }, { visibility: 'hidden' }, { focused: false },
    { windowBlurred: true }, { lifecycle: 'frozen' }, { lifecycle: 'pagehide' }] as Partial<Sample>[]) {
    assert.equal(isActive({ ...visible, ...change }), false);
  }
});

test('blur and visibility events form one departure and one return', () => {
  let model = observe(emptyModel(), visible, 'inicio', 1000, null);
  model = observe(model, { ...visible, focused: false }, 'blur', 1500, true);
  model = observe(model, { ...visible, visibility: 'hidden', hidden: true, focused: false }, 'visibilitychange', 1600, true);
  model = observe(model, { ...visible, focused: false }, 'visibilitychange', 2000, true);
  model = observe(model, visible, 'focus', 2500, true);
  assert.equal(model.exits, 1);
  assert.equal(model.entries, 1);
  assert.equal(model.totalAbsenceMs, 1000);
  assert.equal(model.events[0].absenceMs, 1000);
  assert.equal(model.events[0].direction, 'entrada');
});

test('initial background state is not counted as a departure', () => {
  const model = observe(emptyModel(), { ...visible, focused: false }, 'inicio', 1000, null);
  assert.equal(model.exits, 0);
  assert.equal(model.events[0].direction, 'evento');
});

test('focus inside page does not register a departure', () => {
  let model = observe(emptyModel(), visible, 'inicio', 1000, null);
  model = observe(model, visible, 'focus', 1100, true);
  assert.equal(model.exits, 0);
  assert.equal(model.entries, 0);
});

test('reload preserves logs and counts, not stale focus or open absence', () => {
  let model = observe(emptyModel(), visible, 'inicio', 1000, null);
  model = observe(model, { ...visible, focused: false }, 'blur', 1500, true);
  const restored = restoreModel(JSON.stringify({ ...model, version: 1 }));
  assert.equal(restored.exits, 1);
  assert.deepEqual(restored.events, model.events);
  assert.equal(restored.current, null);
  assert.equal(restored.absentSince, null);
  assert.equal(restored.nextId, 3);
});

test('corrupt and unsupported storage is discarded safely', () => {
  for (const data of [null, '{', '{}', '{"version":2,"events":[]}']) {
    assert.deepEqual(restoreModel(data), emptyModel());
  }
  const restored = restoreModel(JSON.stringify({ version: 1, events: [null, {}, { visibility: 'hidden' }], entries: -2, exits: '2' }));
  assert.deepEqual(restored, emptyModel());
});

test('event retention is bounded without losing total counts', () => {
  let model = observe(emptyModel(), visible, 'inicio', 1000, null);
  for (let index = 0; index < MAX_EVENTS + 10; index++) {
    model = observe(model, { ...visible, focused: index % 2 !== 0 }, 'focus-check', 2000 + index, null);
  }
  assert.equal(model.events.length, MAX_EVENTS);
  assert.equal(model.exits, (MAX_EVENTS + 10) / 2);
  assert.equal(model.entries, (MAX_EVENTS + 10) / 2);
});

test('iframe focus transfer does not masquerade as leaving the page', () => {
  const sample = { ...visible, windowBlurred: true, focusWithinIframe: true };
  assert.equal(isActive(sample), true);
  assert.deepEqual(directReasons(sample), []);
  assert.equal(isActive({ ...sample, focused: false }), false);
  assert.equal(isActive({ ...sample, activeFrameBlurred: true }), false);
});

test('timing and iframe diagnostics cannot change root state or exit counters', () => {
  let model = observe(emptyModel(), visible, 'inicio', 1000, null);
  model = appendDiagnostic(model, 'animation.gap', 1500, null, { evidence: 'indicio', details: 'gap=3000ms' });
  model = appendDiagnostic(model, 'iframe.blur', 2000, true, { scope: 'cross-origin' }, { ...visible, focused: false });
  assert.equal(isActive(model.current), true);
  assert.equal(model.exits, 0);
  assert.equal(model.entries, 0);
  assert.equal(model.events[0].scope, 'cross-origin');
  assert.equal(model.events[1].evidence, 'indicio');
});

test('monotonic absence duration survives a wall-clock adjustment', () => {
  let model = observe(emptyModel(), visible, 'inicio', 1000, null, { monotonicAt: 0 });
  model = observe(model, { ...visible, focused: false }, 'blur', 1500, true, { monotonicAt: 500 });
  model = observe(model, visible, 'focus', 1200, true, { monotonicAt: 2500 });
  assert.equal(model.totalAbsenceMs, 2000);
});

test('timing classifier uses delay, rejects invalid samples and observes exact thresholds', () => {
  assert.equal(classifyTimingGap(1100, 1000, 750), null);
  assert.equal(classifyTimingGap(1750, 1000, 750), 750);
  assert.equal(classifyTimingGap(Infinity, 1000, 750), null);
  assert.equal(classifyTimingGap(-10, 1000, 750), null);
});

test('probe settings are bounded and malformed values use defaults', () => {
  assert.deepEqual(sanitizeSettings(null), defaultSettings);
  const settings = sanitizeSettings({ timerDelayMs: -100, frameTimeoutMs: 1000000,
    inactivitySeconds: Infinity, inactivityEnabled: false, elementFocusEnabled: 'true' });
  assert.equal(settings.timerDelayMs, 100);
  assert.equal(settings.frameTimeoutMs, 60000);
  assert.equal(settings.inactivitySeconds, 30);
  assert.equal(settings.inactivityEnabled, false);
  assert.equal(settings.elementFocusEnabled, true);
});

test('frame protocol rejects malformed payloads and wrong identities', () => {
  const valid = { protocol: 'momor-activity-frame-v1', token: 'secret', id: 'frame', sequence: 1,
    source: 'amostra', trusted: null, at: 1000, sample: visible, activeElement: 'body' };
  assert.equal(validFramePayload(valid, 'secret', 'frame'), true);
  for (const change of [{ token: 'wrong' }, { id: 'wrong' }, { sequence: 0 },
    { sequence: 0.5 }, { at: Infinity }, { sample: null }, { trusted: 'true' }]) {
    assert.equal(validFramePayload({ ...valid, ...change }, 'secret', 'frame'), false);
  }
});

test('session namespaces survive reload and isolate independently opened tabs', () => {
  function storage() {
    const values = new Map<string, string>();
    return { getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => { values.set(key, value); } };
  }
  const first = storage();
  const second = storage();
  const firstKey = sessionStorageKey(first);
  assert.equal(sessionStorageKey(first), firstKey);
  assert.notEqual(sessionStorageKey(second), firstKey);
});

test('legacy logs acquire provenance fields without losing history', () => {
  const legacy = { ...visible, id: 1, at: 1000, source: 'blur', direction: 'saida', active: false, trusted: true, absenceMs: null };
  const model = restoreModel(JSON.stringify({ version: 1, events: [legacy], entries: 0, exits: 1 }));
  assert.equal(model.events[0].evidence, 'direto');
  assert.equal(model.events[0].scope, 'pagina');
  assert.equal(model.exits, 1);
});
