export type Direction = 'entrada' | 'saida' | 'evento';
export type Lifecycle = 'running' | 'pagehide' | 'frozen';
export type Evidence = 'direto' | 'indicio' | 'diagnostico';
export interface Sample {
  visibility: DocumentVisibilityState;
  hidden: boolean;
  focused: boolean;
  windowBlurred: boolean;
  lifecycle: Lifecycle;
  focusWithinIframe?: boolean;
  activeFrameBlurred?: boolean;
}
export interface ActivityEvent extends Sample {
  id: number;
  at: number;
  source: string;
  direction: Direction;
  active: boolean;
  trusted: boolean | null;
  absenceMs: number | null;
  evidence: Evidence;
  scope: string;
  details: string;
  monotonicAt: number | null;
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
  absentMonotonicSince: number | null;
}

export interface EventOptions {
  evidence?: Evidence;
  scope?: string;
  details?: string;
  monotonicAt?: number;
}

export const STORAGE_KEY = 'momor-activity-test-v1';
export const MAX_EVENTS = 2000;
export const POLL_INTERVAL = 250;

export function emptyModel(): ActivityModel {
  return {
    current: null, events: [], entries: 0, exits: 0, totalAbsenceMs: 0,
    absentSince: null, lastExitAt: null, nextId: 1, absentMonotonicSince: null,
  };
}

export function isActive(sample: Sample | null): boolean {
  return sample !== null && !sample.hidden && sample.visibility === 'visible'
    && sample.focused && !sample.activeFrameBlurred
    && (!sample.windowBlurred || sample.focusWithinIframe === true)
    && sample.lifecycle === 'running';
}

export function observe(
  model: ActivityModel, sample: Sample, source: string, at: number, trusted: boolean | null,
  options: EventOptions = {},
): ActivityModel {
  const active = isActive(sample);
  const previousActive = isActive(model.current);
  const direction: Direction = model.current === null || active === previousActive
    ? 'evento' : active ? 'entrada' : 'saida';
  const monotonicAt = options.monotonicAt ?? null;
  const absenceMs = direction === 'entrada' && model.absentSince !== null
    ? Math.max(0, monotonicAt !== null && model.absentMonotonicSince !== null
      ? monotonicAt - model.absentMonotonicSince : at - model.absentSince) : null;
  const absentSince = active ? null : model.absentSince ?? at;
  return {
    current: sample,
    events: [{ ...sample, id: model.nextId, at, source, direction, active, trusted, absenceMs,
      evidence: options.evidence ?? (direction === 'evento' ? 'diagnostico' : 'direto'),
      scope: options.scope ?? 'pagina', details: options.details ?? '', monotonicAt },
      ...model.events].slice(0, MAX_EVENTS),
    entries: model.entries + Number(direction === 'entrada'),
    exits: model.exits + Number(direction === 'saida'),
    totalAbsenceMs: model.totalAbsenceMs + (absenceMs ?? 0),
    absentSince,
    absentMonotonicSince: active ? null : model.absentMonotonicSince ?? monotonicAt,
    lastExitAt: direction === 'saida' ? at : model.lastExitAt,
    nextId: model.nextId + 1,
  };
}

export function appendDiagnostic(
  model: ActivityModel, source: string, at: number, trusted: boolean | null,
  options: EventOptions = {}, sample = model.current,
): ActivityModel {
  if (!sample) return model;
  return {
    ...model,
    events: [{ ...sample, id: model.nextId, at, source, direction: 'evento' as const,
      active: isActive(sample), trusted, absenceMs: null, evidence: options.evidence ?? 'diagnostico',
      scope: options.scope ?? 'pagina', details: options.details ?? '',
      monotonicAt: options.monotonicAt ?? null }, ...model.events].slice(0, MAX_EVENTS),
    nextId: model.nextId + 1,
  };
}

export function classifyTimingGap(intervalMs: number, expectedMs: number, thresholdMs: number): number | null {
  const delay = intervalMs - expectedMs;
  return Number.isFinite(delay) && delay >= thresholdMs ? Math.round(delay) : null;
}

export function directReasons(sample: Sample | null): string[] {
  if (!sample) return [];
  const reasons = [];
  if (sample.visibility !== 'visible') reasons.push(`visibilityState=${sample.visibility}`);
  if (sample.hidden) reasons.push('document.hidden=true');
  if (!sample.focused) reasons.push('document.hasFocus()=false');
  if (sample.windowBlurred && !sample.focusWithinIframe) reasons.push('window.blur');
  if (sample.activeFrameBlurred) reasons.push('activeIframe.blur');
  if (sample.lifecycle !== 'running') reasons.push(`lifecycle=${sample.lifecycle}`);
  return reasons;
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
    if (![1, 2].includes(data.version ?? 0) || !Array.isArray(data.events)) return fallback;
    const events = data.events.filter((event): event is ActivityEvent =>
      isSample(event) && typeof event.source === 'string'
      && Number.isSafeInteger(event.id) && event.id > 0
      && Number.isFinite(event.at) && event.at >= 0
      && ['entrada', 'saida', 'evento'].includes(event.direction)
      && typeof event.active === 'boolean'
      && (typeof event.trusted === 'boolean' || event.trusted === null)
      && (event.absenceMs === null || (Number.isFinite(event.absenceMs) && event.absenceMs >= 0)),
    ).slice(0, MAX_EVENTS).map(event => ({
      ...event,
      evidence: ['direto', 'indicio', 'diagnostico'].includes(event.evidence)
        ? event.evidence : event.direction === 'evento' ? 'diagnostico' : 'direto',
      scope: typeof event.scope === 'string' ? event.scope.slice(0, 80) : 'pagina',
      details: typeof event.details === 'string' ? event.details.slice(0, 1000) : '',
      monotonicAt: typeof event.monotonicAt === 'number' && Number.isFinite(event.monotonicAt)
        ? event.monotonicAt : null,
    }));
    const finiteCount = (value: unknown) => typeof value === 'number'
      && Number.isSafeInteger(value) && value >= 0 ? value : 0;
    return {
      ...fallback, events,
      entries: finiteCount(data.entries), exits: finiteCount(data.exits),
      totalAbsenceMs: typeof data.totalAbsenceMs === 'number' && Number.isFinite(data.totalAbsenceMs)
        && data.totalAbsenceMs >= 0 ? data.totalAbsenceMs : 0,
      lastExitAt: typeof data.lastExitAt === 'number' && Number.isFinite(data.lastExitAt)
        ? data.lastExitAt : null,
      nextId: Math.max(0, ...events.map(event => event.id)) + 1,
    };
  } catch {
    return fallback;
  }
}

export function sessionStorageKey(storage: Pick<Storage, 'getItem' | 'setItem'>): string {
  const key = 'momor-activity-test-tab';
  let identifier = storage.getItem(key);
  if (!identifier || !/^[\w-]{1,80}$/.test(identifier)) {
    identifier = crypto.randomUUID();
    storage.setItem(key, identifier);
  }
  return `momor-activity-test-v2:${identifier}`;
}

export function createActivityStore() {
  let model = emptyModel();
  let storageError = false;
  let lifecycle: Lifecycle = 'running';
  let windowBlurred = false;
  let storageKey = `momor-activity-test-v2:${crypto.randomUUID()}`;
  const listeners = new Set<() => void>();
  const frameSamples = new WeakMap<HTMLIFrameElement, Sample>();
  const ownerId = crypto.randomUUID();
  let namespacePending = false;
  let namespaceChannel: BroadcastChannel | null = null;
  let namespaceTimer = 0;
  try {
    storageKey = sessionStorageKey(sessionStorage);
    const saved = localStorage.getItem(storageKey);
    if (saved) model = restoreModel(saved);
    else if (!localStorage.getItem('momor-activity-test-migrated')) {
      model = restoreModel(localStorage.getItem(STORAGE_KEY));
      localStorage.setItem('momor-activity-test-migrated', 'true');
    }
  } catch {
    storageError = true;
  }

  function sample(): Sample {
    const activeFrame = document.activeElement instanceof HTMLIFrameElement ? document.activeElement : null;
    const activeFrameBlurred = activeFrame ? frameSamples.get(activeFrame)?.windowBlurred === true : false;
    return {
      visibility: document.visibilityState, hidden: document.hidden,
      focused: document.hasFocus(), windowBlurred, lifecycle,
      focusWithinIframe: activeFrame !== null && document.hasFocus() && !activeFrameBlurred,
      activeFrameBlurred,
    };
  }
  function publish() {
    if (!namespacePending) {
      try {
        localStorage.setItem(storageKey, JSON.stringify({ ...model, version: 2 }));
        storageError = false;
      } catch {
        storageError = true;
      }
    }
    listeners.forEach(listener => listener());
  }
  function forkNamespace() {
    const identifier = crypto.randomUUID();
    storageKey = `momor-activity-test-v2:${identifier}`;
    try { sessionStorage.setItem('momor-activity-test-tab', identifier); }
    catch { storageError = true; }
    namespacePending = false;
    model = appendDiagnostic(model, 'namespace.fork', Date.now(), null,
      { details: 'Identidade de aba copiada; historico agora isolado', monotonicAt: performance.now() });
    publish();
  }
  function record(source: string, trusted: boolean | null, options: EventOptions = {}) {
    const current = sample();
    model = observe(model, current, source, Date.now(), trusted, {
      monotonicAt: performance.now(), details: directReasons(current).join('; '), ...options,
    });
    publish();
  }
  function onEvent(event: Event) {
    if (namespacePending && (event.type === 'pagehide' || event.type === 'freeze'
      || (event.type === 'visibilitychange' && document.hidden))) forkNamespace();
    if (event.type === 'blur') windowBlurred = true;
    if (event.type === 'focus' || event.type === 'pageshow') windowBlurred = false;
    if (event.type === 'pagehide') lifecycle = 'pagehide';
    if (event.type === 'freeze') lifecycle = 'frozen';
    if (event.type === 'pageshow' || event.type === 'resume') lifecycle = 'running';
    record(event.type, event.isTrusted, {
      evidence: 'direto',
      details: event instanceof PageTransitionEvent
        ? `persisted=${event.persisted}; ${directReasons(sample()).join('; ')}`
        : directReasons(sample()).join('; '),
    });
  }

  if (typeof BroadcastChannel !== 'undefined') {
    try {
      namespaceChannel = new BroadcastChannel('momor-activity-namespaces-v2');
      namespacePending = true;
      namespaceChannel.onmessage = event => {
        const data: unknown = event.data;
        if (!data || typeof data !== 'object') return;
        const message = data as { type?: string; namespace?: string; owner?: string; target?: string };
        if (message.namespace !== storageKey || typeof message.owner !== 'string'
          || message.owner.length > 80 || message.owner === ownerId) return;
        if (message.type === 'claim') namespaceChannel?.postMessage({ type: 'owner', namespace: storageKey, owner: ownerId, target: message.owner });
        if (message.type === 'owner' && message.target === ownerId && namespacePending) forkNamespace();
      };
      namespaceChannel.postMessage({ type: 'claim', namespace: storageKey, owner: ownerId });
      namespaceTimer = window.setTimeout(() => { namespacePending = false; publish(); }, 150);
    } catch {
      namespacePending = false;
      namespaceChannel?.close();
      namespaceChannel = null;
    }
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
      || next.lifecycle !== model.current.lifecycle
      || next.focusWithinIframe !== model.current.focusWithinIframe
      || next.activeFrameBlurred !== model.current.activeFrameBlurred)) record('verificacao', null, { evidence: 'direto' });
  }, POLL_INTERVAL);

  return {
    getSnapshot: () => model,
    hasStorageError: () => storageError,
    get sessionId() { return storageKey.slice(storageKey.indexOf(':') + 1); },
    setFrameSample(element: HTMLIFrameElement, frameSample: Sample, trusted: boolean | null) {
      frameSamples.set(element, frameSample);
      if (document.activeElement === element && sample().activeFrameBlurred !== model.current?.activeFrameBlurred) {
        record('iframe-focus', trusted, { evidence: 'direto' });
      }
    },
    diagnose(source: string, trusted: boolean | null, options: EventOptions = {}, frameSample?: Sample) {
      model = appendDiagnostic(model, source, Date.now(), trusted,
        { monotonicAt: performance.now(), ...options }, frameSample);
      publish();
    },
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    clear() {
      model = emptyModel();
      record('limpeza', null);
    },
    dispose() {
      if (namespacePending) forkNamespace();
      window.clearTimeout(namespaceTimer);
      namespaceChannel?.close();
      window.clearInterval(timer);
      windowEvents.forEach(type => window.removeEventListener(type, onEvent));
      documentEvents.forEach(type => document.removeEventListener(type, onEvent));
      listeners.clear();
    },
  };
}
