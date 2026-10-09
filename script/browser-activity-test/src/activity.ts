export type Direction = 'entrada' | 'saida' | 'evento';
export type Lifecycle = 'running' | 'pagehide' | 'frozen';
export interface Sample {
  visibility: DocumentVisibilityState;
  hidden: boolean;
  focused: boolean;
  windowBlurred: boolean;
  lifecycle: Lifecycle;
}
export interface ActivityEvent extends Sample {
  id: number;
  at: number;
  source: string;
  direction: Direction;
  active: boolean;
  trusted: boolean | null;
  absenceMs: number | null;
}
export interface ActivityModel {
  current: Sample | null;
  events: ActivityEvent[];
  entries: number;
  exits: number;
  totalAbsenceMs: number;
  absentSince: number | null;
  lastExitAt: number | null;
  nextId: number;
}

export const STORAGE_KEY = 'momor-activity-test-v1';
export const MAX_EVENTS = 500;
export const POLL_INTERVAL = 250;

export function emptyModel(): ActivityModel {
  return {
    current: null, events: [], entries: 0, exits: 0, totalAbsenceMs: 0,
    absentSince: null, lastExitAt: null, nextId: 1,
  };
}

export function isActive(sample: Sample | null): boolean {
  return sample !== null && !sample.hidden && sample.visibility === 'visible'
    && sample.focused && !sample.windowBlurred && sample.lifecycle === 'running';
}

export function observe(
  model: ActivityModel, sample: Sample, source: string, at: number, trusted: boolean | null,
): ActivityModel {
  const active = isActive(sample);
  const previousActive = isActive(model.current);
  const direction: Direction = model.current === null || active === previousActive
    ? 'evento' : active ? 'entrada' : 'saida';
  const absenceMs = direction === 'entrada' && model.absentSince !== null
    ? Math.max(0, at - model.absentSince) : null;
  const absentSince = active ? null : model.absentSince ?? at;
  return {
    current: sample,
    events: [{ ...sample, id: model.nextId, at, source, direction, active, trusted, absenceMs },
      ...model.events].slice(0, MAX_EVENTS),
    entries: model.entries + Number(direction === 'entrada'),
    exits: model.exits + Number(direction === 'saida'),
    totalAbsenceMs: model.totalAbsenceMs + (absenceMs ?? 0),
    absentSince,
    lastExitAt: direction === 'saida' ? at : model.lastExitAt,
    nextId: model.nextId + 1,
  };
}

function isSample(value: unknown): value is Sample {
  if (!value || typeof value !== 'object') return false;
  const sample = value as Partial<Sample>;
  return (sample.visibility === 'visible' || sample.visibility === 'hidden')
    && typeof sample.hidden === 'boolean' && typeof sample.focused === 'boolean'
    && typeof sample.windowBlurred === 'boolean'
    && ['running', 'pagehide', 'frozen'].includes(sample.lifecycle ?? '');
}

export function restoreModel(serialized: string | null): ActivityModel {
  const fallback = emptyModel();
  if (!serialized) return fallback;
  try {
    const saved: unknown = JSON.parse(serialized);
    if (!saved || typeof saved !== 'object') return fallback;
    const data = saved as Partial<ActivityModel> & { version?: number };
    if (data.version !== 1 || !Array.isArray(data.events)) return fallback;
    const events = data.events.filter((event): event is ActivityEvent =>
      isSample(event) && typeof event.source === 'string'
      && Number.isSafeInteger(event.id) && event.id > 0
      && Number.isFinite(event.at) && event.at >= 0
      && ['entrada', 'saida', 'evento'].includes(event.direction)
      && typeof event.active === 'boolean'
      && (typeof event.trusted === 'boolean' || event.trusted === null)
      && (event.absenceMs === null || (Number.isFinite(event.absenceMs) && event.absenceMs >= 0)),
    ).slice(0, MAX_EVENTS);
    const finiteCount = (value: unknown) => typeof value === 'number'
      && Number.isSafeInteger(value) && value >= 0 ? value : 0;
    return {
      ...fallback, events,
      entries: finiteCount(data.entries), exits: finiteCount(data.exits),
      totalAbsenceMs: finiteCount(data.totalAbsenceMs),
      lastExitAt: typeof data.lastExitAt === 'number' && Number.isFinite(data.lastExitAt)
        ? data.lastExitAt : null,
      nextId: Math.max(0, ...events.map(event => event.id)) + 1,
    };
  } catch {
    return fallback;
  }
}

export function createActivityStore() {
  let model = emptyModel();
  let storageError = false;
  let lifecycle: Lifecycle = 'running';
  let windowBlurred = false;
  const listeners = new Set<() => void>();
  try {
    model = restoreModel(localStorage.getItem(STORAGE_KEY));
  } catch {
    storageError = true;
  }

  function sample(): Sample {
    return {
      visibility: document.visibilityState, hidden: document.hidden,
      focused: document.hasFocus(), windowBlurred, lifecycle,
    };
  }
  function publish() {
    try {
      localStorage.setItem(STORAGE_KEY, JSON.stringify({ ...model, version: 1 }));
      storageError = false;
    } catch {
      storageError = true;
    }
    listeners.forEach(listener => listener());
  }
  function record(source: string, trusted: boolean | null) {
    model = observe(model, sample(), source, Date.now(), trusted);
    publish();
  }
  function onEvent(event: Event) {
    if (event.type === 'blur') windowBlurred = true;
    if (event.type === 'focus' || event.type === 'pageshow') windowBlurred = false;
    if (event.type === 'pagehide') lifecycle = 'pagehide';
    if (event.type === 'freeze') lifecycle = 'frozen';
    if (event.type === 'pageshow' || event.type === 'resume') lifecycle = 'running';
    record(event.type, event.isTrusted);
  }

  const windowEvents = ['focus', 'blur', 'pageshow', 'pagehide'];
  const documentEvents = ['visibilitychange', 'freeze', 'resume'];
  windowEvents.forEach(type => window.addEventListener(type, onEvent));
  documentEvents.forEach(type => document.addEventListener(type, onEvent));
  record('inicio', null);
  const timer = window.setInterval(() => {
    const next = sample();
    if (model.current && (next.focused !== model.current.focused
      || next.hidden !== model.current.hidden || next.visibility !== model.current.visibility
      || next.lifecycle !== model.current.lifecycle)) record('verificacao', null);
  }, POLL_INTERVAL);

  return {
    getSnapshot: () => model,
    hasStorageError: () => storageError,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    clear() {
      model = emptyModel();
      record('limpeza', null);
    },
    dispose() {
      window.clearInterval(timer);
      windowEvents.forEach(type => window.removeEventListener(type, onEvent));
      documentEvents.forEach(type => document.removeEventListener(type, onEvent));
      listeners.clear();
    },
  };
}
