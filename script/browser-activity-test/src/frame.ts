import './style.css';
import type { Lifecycle, Sample } from './activity';

const parameters = new URLSearchParams(location.search);
const identifier = parameters.get('id') ?? '';
const token = parameters.get('token') ?? '';
const parentOrigin = parameters.get('parentOrigin') ?? '';
let lifecycle: Lifecycle = 'running';
let windowBlurred = false;
let sequence = 0;
let lastInteractionReport = -Infinity;
const origin = document.getElementById('frame-origin');
if (origin) origin.textContent = location.origin;

function report(source: string, trusted: boolean | null) {
  const sample: Sample = { visibility: document.visibilityState, hidden: document.hidden,
    focused: document.hasFocus(), windowBlurred, lifecycle };
  const status = document.getElementById('frame-status');
  const signals = document.getElementById('frame-signals');
  const activeElement = document.activeElement;
  const descriptor = activeElement
    ? `${activeElement.tagName.toLowerCase()}${activeElement.id ? `#${activeElement.id}` : ''}` : '-';
  if (status) {
    status.textContent = sample.hidden ? 'Oculto' : sample.focused ? 'Com foco' : 'Sem foco neste frame';
    status.dataset.state = sample.hidden ? 'hidden' : sample.focused ? 'focused' : 'passive';
  }
  if (signals) signals.textContent = `${sample.visibility} / hasFocus=${sample.focused} / ${sample.lifecycle}`;
  if (window.parent === window || !/^https?:\/\/(localhost|127\.0\.0\.1)(:\d+)?$/.test(parentOrigin)) return;
  window.parent.postMessage({ protocol: 'momor-activity-frame-v1', id: identifier, token,
    sequence: ++sequence, at: Date.now(), source, trusted, sample, activeElement: descriptor }, parentOrigin);
}
function eventListener(event: Event) {
  if (event.type === 'blur') windowBlurred = true;
  if (event.type === 'focus' || event.type === 'pageshow') windowBlurred = false;
  if (event.type === 'pagehide') lifecycle = 'pagehide';
  if (event.type === 'freeze') lifecycle = 'frozen';
  if (event.type === 'pageshow' || event.type === 'resume') lifecycle = 'running';
  report(event.type, event.isTrusted);
}
for (const type of ['focus', 'blur', 'pageshow', 'pagehide']) window.addEventListener(type, eventListener);
for (const type of ['visibilitychange', 'freeze', 'resume', 'focusin', 'focusout']) document.addEventListener(type, eventListener);
for (const type of ['pointermove', 'pointerdown', 'keydown', 'wheel']) document.addEventListener(type, event => {
  if (event.isTrusted && performance.now() - lastInteractionReport >= 2000) {
    lastInteractionReport = performance.now();
    report('interacao', true);
  }
}, { passive: true });
report('inicio', null);
setInterval(() => report('amostra', null), 500);
