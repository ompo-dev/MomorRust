import assert from 'node:assert/strict';
import { test } from 'node:test';
import { emptyModel, isActive, MAX_EVENTS, observe, restoreModel } from './activity.ts';
import type { Sample } from './activity.ts';

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
  assert.equal(model.exits, 255);
  assert.equal(model.entries, 255);
});
