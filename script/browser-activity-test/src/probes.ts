import { classifyTimingGap, isActive } from './activity.ts';
import type { createActivityStore, Sample } from './activity.ts';

export interface ProbeSettings {
  timerDelayMs: number;
  animationGapMs: number;
  workerDelayMs: number;
  serverDelayMs: number;
  frameTimeoutMs: number;
  inactivitySeconds: number;
  inactivityEnabled: boolean;
  elementFocusEnabled: boolean;
}
export const defaultSettings: ProbeSettings = {
  timerDelayMs: 750, animationGapMs: 1200, workerDelayMs: 1500,
  serverDelayMs: 3000, frameTimeoutMs: 5000, inactivitySeconds: 30, inactivityEnabled: true, elementFocusEnabled: true,
};
export const settingRanges = {
  timerDelayMs: [100, 60000], animationGapMs: [100, 60000], workerDelayMs: [100, 60000],
  frameTimeoutMs: [2000, 60000], inactivitySeconds: [5, 3600],
  serverDelayMs: [100, 60000],
} as const;
export function sanitizeSettings(value: unknown): ProbeSettings {
  const result = { ...defaultSettings };
  if (!value || typeof value !== 'object') return result;
  const candidate = value as Partial<ProbeSettings>;
  for (const key of Object.keys(settingRanges) as (keyof typeof settingRanges)[]) {
    const number = candidate[key];
    if (typeof number === 'number' && Number.isFinite(number)) {
      const [minimum, maximum] = settingRanges[key];
      result[key] = Math.max(minimum, Math.min(maximum, Math.round(number)));
    }
  }
  for (const key of ['inactivityEnabled', 'elementFocusEnabled'] as const) {
    if (typeof candidate[key] === 'boolean') result[key] = candidate[key];
  }
  return result;
}

export interface FramePayload {
  protocol: 'momor-activity-frame-v1';
  token: string;
  id: string;
  sequence: number;
  source: string;
  trusted: boolean | null;
  at: number;
  sample: Sample;
  activeElement: string;
}
export function validFramePayload(value: unknown, token: string, id: string): value is FramePayload {
  if (!value || typeof value !== 'object') return false;
  const data = value as Partial<FramePayload>;
  const sample = data.sample;
  return data.protocol === 'momor-activity-frame-v1' && data.token === token && data.id === id
    && typeof data.sequence === 'number' && Number.isSafeInteger(data.sequence) && data.sequence > 0
    && typeof data.at === 'number' && Number.isFinite(data.at)
    && typeof data.source === 'string' && data.source.length <= 40
    && typeof data.activeElement === 'string' && data.activeElement.length <= 100
    && (data.trusted === null || typeof data.trusted === 'boolean')
    && sample !== undefined && typeof sample === 'object' && sample !== null
    && ['visible', 'hidden'].includes(sample.visibility)
    && typeof sample.hidden === 'boolean' && typeof sample.focused === 'boolean'
    && typeof sample.windowBlurred === 'boolean'
    && ['running', 'pagehide', 'frozen'].includes(sample.lifecycle);
}

export interface FrameReading {
  id: string;
  origin: string;
  sample: Sample | null;
  activeElement: string;
  receivedAt: number | null;
  heartbeatCount: number;
  sequence: number;
  status: 'waiting' | 'connected' | 'late';
}
interface RegisteredFrame extends FrameReading {
  element: HTMLIFrameElement;
  registeredAt: number;
  receivedMonotonicAt: number | null;
}
export interface AlertReading { id: string; details: string; at: number; expiresAt: number | null }
export interface ProbeSnapshot {
  settings: ProbeSettings;
  settingsStorageError: boolean;
  timerDelayMs: number;
  maxTimerDelayMs: number;
  animationGapMs: number;
  maxAnimationGapMs: number;
  framesPerSecond: number;
  workerDelayMs: number | null;
  workerDeliveryMs: number | null;
  workerCount: number;
  serverIntervalMs: number | null;
  serverMaxGapMs: number;
  serverRoundTripMs: number | null;
  serverCount: number;
  longTaskCount: number;
  longestTaskMs: number;
  interactionCount: number;
  inactiveMs: number;
  pointerInside: boolean | null;
  activeElement: string;
  viewport: string;
  visualViewport: string;
  fullscreen: boolean;
  pointerLocked: boolean;
  online: boolean;
  prerendering: boolean;
  wasDiscarded: boolean;
  navigationType: string;
  alerts: AlertReading[];
  hintCount: number;
  frames: FrameReading[];
  capabilities: Record<string, boolean>;
  samples: { at: number; active: boolean; hints: number; timerDelayMs: number }[];
}

export function describeElement(element: EventTarget | null): string {
  if (!(element instanceof Element)) return '-';
  return `${element.tagName.toLowerCase()}${element.id ? `#${element.id.slice(0, 60)}` : ''}`;
}

export function createProbeStore(activity: ReturnType<typeof createActivityStore>) {
  const listeners = new Set<() => void>();
  const cleanups: (() => void)[] = [];
  const alerts = new Map<string, AlertReading>();
  const registeredFrames = new Map<string, RegisteredFrame>();
  const token = crypto.randomUUID();
  const settingsKey = `momor-activity-probes:${activity.sessionId}`;
  const now = performance.now();
  let previousTick = now;
  let previousWallTick = Date.now();
  let lastAnimation = now;
  let lastInteraction = now;
  let lastInteractionLog = -Infinity;
  let animationCount = 0;
  let previousAnimationCount = 0;
  let animationWindowMax = 0;
  let lastWorker = now;
  let lastLongTask = -Infinity;
  let disposed = false;
  let settings = { ...defaultSettings };
  let settingsStorageError = false;
  try { settings = sanitizeSettings(JSON.parse(localStorage.getItem(settingsKey) ?? 'null')); }
  catch { settingsStorageError = true; }
  const extendedDocument = document as Document & { prerendering?: boolean; wasDiscarded?: boolean };
  let state: ProbeSnapshot = {
    settings, settingsStorageError, timerDelayMs: 0, maxTimerDelayMs: 0,
    animationGapMs: 0, maxAnimationGapMs: 0, framesPerSecond: 0,
    workerDelayMs: null, workerDeliveryMs: null, workerCount: 0, serverIntervalMs: null,
    serverMaxGapMs: 0, serverRoundTripMs: null, serverCount: 0, longTaskCount: 0, longestTaskMs: 0,
    interactionCount: 0, inactiveMs: 0, pointerInside: null, activeElement: '-', viewport: '-',
    visualViewport: '-', fullscreen: false, pointerLocked: false, online: navigator.onLine,
    prerendering: extendedDocument.prerendering ?? false, wasDiscarded: extendedDocument.wasDiscarded ?? false,
    navigationType: (performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming | undefined)?.type ?? '-',
    alerts: [], hintCount: 0, frames: [], samples: [],
    capabilities: {
      Worker: typeof Worker !== 'undefined',
      LongTasks: typeof PerformanceObserver !== 'undefined' && PerformanceObserver.supportedEntryTypes.includes('longtask'),
      VisualViewport: window.visualViewport !== null,
      Fullscreen: document.fullscreenEnabled,
      PageFreeze: 'onfreeze' in document,
      Prerendering: 'prerendering' in document,
      WasDiscarded: 'wasDiscarded' in document,
      ServerHeartbeat: false,
    },
  };
  let snapshot: ProbeSnapshot = { ...state, capabilities: { ...state.capabilities } };
  function emit() {
    if (disposed) return;
    state = { ...state, settings, alerts: [...alerts.values()], frames: [...registeredFrames.values()].map(frame => ({
      id: frame.id, origin: frame.origin, sample: frame.sample, activeElement: frame.activeElement,
      receivedAt: frame.receivedAt, heartbeatCount: frame.heartbeatCount, sequence: frame.sequence, status: frame.status,
    })) };
    snapshot = { ...state, capabilities: { ...state.capabilities } };
    listeners.forEach(listener => listener());
  }
  function hint(id: string, details: string, ttl: number | null = 5000) {
    const existing = alerts.get(id);
    const at = Date.now();
    if (!existing || at - existing.at >= 5000) {
      activity.diagnose(id, null, { evidence: 'indicio', scope: 'sondas', details });
      state.hintCount += 1;
    }
    alerts.set(id, { id, details, at: existing && at - existing.at < 5000 ? existing.at : at,
      expiresAt: ttl === null ? null : at + ttl });
  }
  function listen(target: EventTarget, type: string, callback: EventListener, options?: AddEventListenerOptions) {
    target.addEventListener(type, callback, options);
    cleanups.push(() => target.removeEventListener(type, callback, options));
  }
  const diagnostic: EventListener = event => {
    if ((event.type === 'focusin' || event.type === 'focusout') && !settings.elementFocusEnabled) return;
    const focusDetails = event instanceof FocusEvent
      ? `target=${describeElement(event.target)}; related=${describeElement(event.relatedTarget)}` : '';
    activity.diagnose(event.type, event.isTrusted, { details: focusDetails ||
      `fullscreen=${Boolean(document.fullscreenElement)}; pointerLock=${Boolean(document.pointerLockElement)}; online=${navigator.onLine}` });
  };
  for (const type of ['focusin', 'focusout', 'fullscreenchange', 'pointerlockchange',
    'prerenderingchange', 'copy', 'cut', 'paste', 'contextmenu']) listen(document, type, diagnostic);
  for (const type of ['online', 'offline', 'popstate', 'hashchange']) listen(window, type, diagnostic);

  const interaction: EventListener = event => {
    if (!event.isTrusted) return;
    lastInteraction = performance.now();
    state.interactionCount += 1;
    alerts.delete('input.inactive');
    if (lastInteraction - lastInteractionLog >= 2000) {
      lastInteractionLog = lastInteraction;
      activity.diagnose('interacao', true, { details: `type=${event.type}; target=${describeElement(event.target)}` });
    }
  };
  for (const type of ['keydown', 'pointerdown', 'pointermove', 'wheel', 'touchstart']) {
    listen(document, type, interaction, { passive: true, capture: true });
  }
  listen(document.documentElement, 'pointerleave', event => {
    state.pointerInside = false;
    hint('pointer.outside', `event=${event.type}; isTrusted=${event.isTrusted}`, null);
    emit();
  });
  listen(document.documentElement, 'pointerenter', event => {
    state.pointerInside = true;
    alerts.delete('pointer.outside');
    activity.diagnose('pointerenter', event.isTrusted);
    emit();
  });
  let resizeTimer = 0;
  const resize: EventListener = event => {
    window.clearTimeout(resizeTimer);
    resizeTimer = window.setTimeout(() => activity.diagnose(event.type, event.isTrusted,
      { details: `viewport=${innerWidth}x${innerHeight}; scale=${window.visualViewport?.scale ?? '-'}` }), 150);
  };
  listen(window, 'resize', resize);
  if (window.visualViewport) {
    listen(window.visualViewport, 'resize', resize);
    listen(window.visualViewport, 'scroll', resize);
  }
  cleanups.push(() => window.clearTimeout(resizeTimer));

  let animationRequest = 0;
  function animation(at: number) {
    if (disposed) return;
    const gap = Math.max(0, at - lastAnimation);
    animationWindowMax = Math.max(animationWindowMax, gap);
    lastAnimation = at;
    animationCount += 1;
    animationRequest = requestAnimationFrame(animation);
  }
  animationRequest = requestAnimationFrame(animation);
  cleanups.push(() => cancelAnimationFrame(animationRequest));

  let worker: Worker | null = null;
  if (state.capabilities.Worker) {
    try {
      worker = new Worker(new URL('./heartbeat.worker.ts', import.meta.url), { type: 'module' });
      worker.onmessage = event => {
        const data: unknown = event.data;
        if (!data || typeof data !== 'object') return;
        const reading = data as { sequence?: unknown; epochAt?: unknown; intervalMs?: unknown };
        if (typeof reading.sequence !== 'number' || !Number.isSafeInteger(reading.sequence)
          || reading.sequence <= state.workerCount || typeof reading.epochAt !== 'number'
          || !Number.isFinite(reading.epochAt) || typeof reading.intervalMs !== 'number'
          || !Number.isFinite(reading.intervalMs)) return;
        lastWorker = performance.now();
        state.workerCount = reading.sequence;
        state.workerDelayMs = Math.max(0, Math.round(reading.intervalMs - 1000));
        state.workerDeliveryMs = Math.max(0, Math.round(performance.timeOrigin + performance.now() - reading.epochAt));
        if (state.workerDelayMs >= settings.workerDelayMs || state.workerDeliveryMs >= settings.workerDelayMs) {
          hint('worker.delay', `timerDelay=${state.workerDelayMs}ms; deliveryDelay=${state.workerDeliveryMs}ms`);
        }
      };
      worker.onerror = event => {
        state.capabilities.Worker = false;
        activity.diagnose('worker.error', event.isTrusted, { scope: 'sondas', details: 'Worker indisponivel' });
        worker?.terminate();
        emit();
      };
    } catch {
      state.capabilities.Worker = false;
      activity.diagnose('worker.unavailable', null, { scope: 'sondas' });
    }
  }
  cleanups.push(() => worker?.terminate());

  let requestInFlight = false;
  let requestController: AbortController | null = null;
  async function serverHeartbeat() {
    if (disposed || requestInFlight) return;
    requestInFlight = true;
    requestController = new AbortController();
    const timeout = window.setTimeout(() => requestController?.abort(), 5000);
    const startedAt = performance.now();
    try {
      const response = await fetch('/api/activity-heartbeat', { method: 'POST', cache: 'no-store',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ id: `${activity.sessionId}.${token}` }), signal: requestController.signal });
      if (!response.ok) throw new Error('Heartbeat unavailable');
      const data: unknown = await response.json();
      if (!data || typeof data !== 'object') throw new Error('Invalid heartbeat');
      const reading = data as { protocol?: string; intervalMs?: number | null; maxGapMs?: number; count?: number };
      if (reading.protocol !== 'momor-heartbeat-v1' || !Number.isFinite(reading.maxGapMs)
        || !Number.isSafeInteger(reading.count) || (reading.count ?? 0) < 1
        || (reading.intervalMs !== null && !Number.isFinite(reading.intervalMs))) throw new Error('Invalid heartbeat');
      if (disposed) return;
      state.capabilities.ServerHeartbeat = true;
      state.serverIntervalMs = reading.intervalMs ?? null;
      state.serverMaxGapMs = reading.maxGapMs ?? 0;
      state.serverCount = reading.count ?? 0;
      state.serverRoundTripMs = Math.round(performance.now() - startedAt);
      alerts.delete('network.unavailable');
      if (state.serverIntervalMs !== null && classifyTimingGap(state.serverIntervalMs, 2000, settings.serverDelayMs) !== null) {
        hint('network.gap', `serverInterval=${state.serverIntervalMs}ms; expected=2000ms; rtt=${state.serverRoundTripMs}ms`);
      }
      emit();
    } catch {
      if (!disposed) {
        state.capabilities.ServerHeartbeat = false;
        hint('network.unavailable', 'Heartbeat local indisponivel; motivo indeterminado');
        emit();
      }
    } finally {
      window.clearTimeout(timeout);
      requestInFlight = false;
    }
  }
  const heartbeatTimer = window.setInterval(() => { void serverHeartbeat(); }, 2000);
  void serverHeartbeat();
  cleanups.push(() => { window.clearInterval(heartbeatTimer); requestController?.abort(); });

  if (state.capabilities.LongTasks) {
    try {
      const observer = new PerformanceObserver(list => {
        for (const entry of list.getEntries()) {
          state.longTaskCount += 1;
          state.longestTaskMs = Math.max(state.longestTaskMs, Math.round(entry.duration));
          lastLongTask = performance.now();
          if (entry.duration >= 200) hint('main.longtask', `duration=${Math.round(entry.duration)}ms; blocking=true`);
        }
      });
      observer.observe({ type: 'longtask', buffered: true });
      cleanups.push(() => observer.disconnect());
    } catch {
      state.capabilities.LongTasks = false;
      activity.diagnose('longtasks.unavailable', null, { scope: 'sondas' });
    }
  }

  listen(window, 'message', event => {
    if (!(event instanceof MessageEvent)) return;
    for (const frame of registeredFrames.values()) {
      if (event.source !== frame.element.contentWindow || event.origin !== frame.origin
        || !validFramePayload(event.data, token, frame.id) || event.data.sequence <= frame.sequence) continue;
      const data = event.data;
      const changed = !frame.sample || frame.sample.visibility !== data.sample.visibility
        || frame.sample.hidden !== data.sample.hidden || frame.sample.focused !== data.sample.focused
        || frame.sample.windowBlurred !== data.sample.windowBlurred || frame.sample.lifecycle !== data.sample.lifecycle;
      frame.sample = data.sample;
      activity.setFrameSample(frame.element, data.sample, data.trusted);
      frame.activeElement = data.activeElement;
      frame.receivedAt = Date.now();
      frame.receivedMonotonicAt = performance.now();
      frame.heartbeatCount += 1;
      frame.sequence = data.sequence;
      frame.status = 'connected';
      if (data.source === 'interacao' && data.trusted) {
        lastInteraction = performance.now();
        state.interactionCount += 1;
        alerts.delete('input.inactive');
      }
      alerts.delete(`iframe.${frame.id}.late`);
      if (changed || data.source !== 'amostra') {
        activity.diagnose(`iframe.${data.source}`, data.trusted, { scope: frame.id,
          evidence: 'diagnostico', details: `origin=${frame.origin}; activeElement=${data.activeElement}; reportedAt=${data.at}` }, data.sample);
      }
      if (data.sample.hidden && isActive(activity.getSnapshot().current)) {
        hint(`iframe.${frame.id}.hidden`, `frame=${frame.id}; parent=visible; iframe=hidden`);
      } else alerts.delete(`iframe.${frame.id}.hidden`);
      if (changed) emit();
    }
  });

  function tick() {
    const at = performance.now();
    const wall = Date.now();
    const interval = at - previousTick;
    const delay = classifyTimingGap(interval, 1000, settings.timerDelayMs);
    state.timerDelayMs = Math.max(0, Math.round(interval - 1000));
    state.maxTimerDelayMs = Math.max(state.maxTimerDelayMs, state.timerDelayMs);
    state.animationGapMs = Math.round(Math.max(animationWindowMax, at - lastAnimation));
    state.maxAnimationGapMs = Math.max(state.maxAnimationGapMs, state.animationGapMs);
    state.framesPerSecond = Math.round((animationCount - previousAnimationCount) * 1000 / Math.max(1, interval));
    previousAnimationCount = animationCount;
    animationWindowMax = 0;
    state.inactiveMs = Math.round(at - lastInteraction);
    state.activeElement = describeElement(document.activeElement);
    state.viewport = `${innerWidth} x ${innerHeight}`;
    const viewport = window.visualViewport;
    state.visualViewport = viewport ? `${Math.round(viewport.width)} x ${Math.round(viewport.height)} / ${viewport.scale.toFixed(2)}` : '-';
    state.fullscreen = Boolean(document.fullscreenElement);
    state.pointerLocked = Boolean(document.pointerLockElement);
    state.online = navigator.onLine;
    state.prerendering = extendedDocument.prerendering ?? false;
    if (delay !== null) hint('main.timer', `delay=${delay}ms; interval=${Math.round(interval)}ms; longTaskRecent=${at - lastLongTask < 3000}`);
    if (state.animationGapMs >= settings.animationGapMs) hint('animation.gap', `gap=${state.animationGapMs}ms; fps=${state.framesPerSecond}`);
    if (state.capabilities.Worker && at - lastWorker >= settings.workerDelayMs + 1000) {
      hint('worker.silence', `silence=${Math.round(at - lastWorker)}ms`);
    }
    const clockDifference = Math.round((wall - previousWallTick) - interval);
    if (Math.abs(clockDifference) >= 2000) hint('clock.difference', `wallMinusMonotonic=${clockDifference}ms`);
    previousTick = at;
    previousWallTick = wall;
    if (settings.inactivityEnabled && state.inactiveMs >= settings.inactivitySeconds * 1000) {
      if (!alerts.has('input.inactive')) hint('input.inactive', `noTrustedInput=${state.inactiveMs}ms`, null);
    } else alerts.delete('input.inactive');
    for (const frame of registeredFrames.values()) {
      const silence = at - (frame.receivedMonotonicAt ?? frame.registeredAt);
      if (silence >= settings.frameTimeoutMs) {
        frame.status = 'late';
        hint(`iframe.${frame.id}.late`, `frame=${frame.id}; silence=${Math.round(silence)}ms`);
      }
    }
    for (const [id, alert] of alerts) if (alert.expiresAt !== null && alert.expiresAt <= wall) alerts.delete(id);
    state.samples = [...state.samples, { at: wall, active: isActive(activity.getSnapshot().current),
      hints: alerts.size, timerDelayMs: state.timerDelayMs }].slice(-90);
    emit();
  }
  const timer = window.setInterval(tick, 1000);
  cleanups.push(() => window.clearInterval(timer));
  tick();
  activity.diagnose('navigation', null, { details: `type=${state.navigationType}; wasDiscarded=${state.wasDiscarded}; prerendering=${state.prerendering}` });

  function frameUrl(id: string, crossOrigin: boolean): string {
    const url = new URL('/frame.html', location.href);
    if (crossOrigin) url.hostname = location.hostname === 'localhost' ? '127.0.0.1' : 'localhost';
    url.searchParams.set('id', id);
    url.searchParams.set('token', token);
    url.searchParams.set('parentOrigin', location.origin);
    return url.href;
  }

  return {
    getSnapshot: () => snapshot,
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener); }; },
    frameUrl,
    registerFrame(element: HTMLIFrameElement, id: string) {
      registeredFrames.set(id, { element, id, origin: new URL(element.src).origin, sample: null,
        activeElement: '-', registeredAt: performance.now(), receivedMonotonicAt: null,
        receivedAt: null, heartbeatCount: 0, sequence: 0, status: 'waiting' });
      emit();
      return () => {
        registeredFrames.delete(id);
        alerts.delete(`iframe.${id}.late`);
        alerts.delete(`iframe.${id}.hidden`);
        emit();
      };
    },
    updateSettings(next: Partial<ProbeSettings>) {
      settings = sanitizeSettings({ ...settings, ...next });
      try { localStorage.setItem(`momor-activity-probes:${activity.sessionId}`, JSON.stringify(settings)); state.settingsStorageError = false; }
      catch { state.settingsStorageError = true; }
      activity.diagnose('configuracao', null, { scope: 'sondas', details: JSON.stringify(settings) });
      emit();
    },
    clear() {
      alerts.clear();
      state = { ...state, samples: [], hintCount: 0, maxTimerDelayMs: 0, maxAnimationGapMs: 0,
        longestTaskMs: 0, longTaskCount: 0, interactionCount: 0 };
      lastInteraction = performance.now();
      emit();
    },
    dispose() { disposed = true; cleanups.forEach(cleanup => cleanup()); listeners.clear(); },
  };
}
