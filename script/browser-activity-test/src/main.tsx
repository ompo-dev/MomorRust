import { useEffect, useRef, useState, useSyncExternalStore } from 'react';
import { createRoot } from 'react-dom/client';
import { Activity, ArrowDownLeft, ArrowUpRight, Check, Download, Eye, Focus,
  History, Search, ShieldCheck, ShieldX, Trash2, TriangleAlert, Cpu, Timer,
  MousePointer2, Keyboard, Globe, ChevronLeft, ChevronRight, FileSpreadsheet, Maximize2 } from 'lucide-react';
import { createActivityStore, directReasons, isActive, MAX_EVENTS, POLL_INTERVAL } from './activity';
import { createProbeStore, settingRanges } from './probes';
import type { FrameReading, ProbeSettings } from './probes';
import './style.css';

const store = createActivityStore();
const probes = createProbeStore(store);
const sameFrameUrl = probes.frameUrl('same-origin', false);
const crossFrameUrl = probes.frameUrl('cross-origin', true);
if (import.meta.hot) import.meta.hot.dispose(() => { probes.dispose(); store.dispose(); });

const clock = new Intl.DateTimeFormat('pt-BR', {
  hour: '2-digit', minute: '2-digit', second: '2-digit', fractionalSecondDigits: 3,
});
const date = new Intl.DateTimeFormat('pt-BR', { day: '2-digit', month: '2-digit' });
const labels = { entrada: 'Entrada', saida: 'Sa\u00edda', evento: 'Evento' };
function duration(ms: number) {
  return ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`;
}
const evidenceLabels = { direto: 'Direto', indicio: 'Ind\u00edcio', diagnostico: 'Diagn\u00f3stico' };
const pageSize = 100;

function saveFile(contents: string, mime: string, extension: string) {
  const url = URL.createObjectURL(new Blob([contents], { type: mime }));
  const link = document.createElement('a');
  link.href = url;
  link.download = `momor-atividade-${new Date().toISOString().replace(/[:.]/g, '-')}.${extension}`;
  link.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}
function csvCell(value: unknown) {
  const text = String(value ?? '');
  return `"${(/^[=+@-]/.test(text) ? "'" : '') + text.replace(/"/g, '""')}"`;
}

function App() {
  const model = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const readings = useSyncExternalStore(probes.subscribe, probes.getSnapshot);
  const [filter, setFilter] = useState<'todos' | 'saida' | 'entrada' | 'indicio'>('todos');
  const [query, setQuery] = useState('');
  const [view, setView] = useState<'signals' | 'frames' | 'settings'>('signals');
  const [page, setPage] = useState(0);
  const active = isActive(model.current);
  const rows = model.events.filter(event => (filter === 'todos' || filter === event.direction || filter === event.evidence)
    && `${event.source} ${event.scope} ${event.details} ${labels[event.direction]} ${evidenceLabels[event.evidence]}`.toLowerCase().includes(query.toLowerCase()));
  const lastPage = Math.max(0, Math.ceil(rows.length / pageSize) - 1);
  const currentPage = Math.min(page, lastPage);
  const visibleRows = rows.slice(currentPage * pageSize, (currentPage + 1) * pageSize);
  const StatusIcon = active ? ShieldCheck : ShieldX;
  const reason = model.current?.lifecycle !== 'running' ? 'Ciclo da p\u00e1gina interrompido'
    : model.current?.hidden || model.current?.visibility !== 'visible' ? 'P\u00e1gina oculta'
    : model.current?.activeFrameBlurred ? 'Iframe ativo perdeu foco'
    : model.current?.windowBlurred && !model.current.focusWithinIframe ? 'Evento blur observado'
    : !model.current?.focused ? 'Janela sem foco' : 'Vis\u00edvel e com foco';

  function download() {
    saveFile(JSON.stringify({ version: 2, exportedAt: new Date().toISOString(), sessionId: store.sessionId,
      url: location.href, userAgent: navigator.userAgent, ...model, probes: readings }, null, 2), 'application/json', 'json');
  }
  function downloadCsv() {
    const header = ['at', 'monotonicMs', 'movement', 'source', 'evidence', 'scope', 'visibility', 'hasFocus', 'hidden', 'lifecycle', 'isTrusted', 'absenceMs', 'details'];
    const values = model.events.map(event => [new Date(event.at).toISOString(), event.monotonicAt,
      event.direction, event.source, event.evidence, event.scope, event.visibility, event.focused,
      event.hidden, event.lifecycle, event.trusted, event.absenceMs, event.details]);
    saveFile([header, ...values].map(row => row.map(csvCell).join(',')).join('\r\n'), 'text/csv;charset=utf-8', 'csv');
  }
  async function toggleFullscreen() {
    try {
      if (document.fullscreenElement) await document.exitFullscreen();
      else await document.documentElement.requestFullscreen();
    } catch {
      store.diagnose('fullscreen.error', null, { details: 'Solicitacao indisponivel ou recusada' });
    }
  }

  return (
    <main data-status={active ? 'active' : 'away'}>
      <header className="border-b border-zinc-200 bg-white">
        <div className="shell flex flex-wrap items-center justify-between gap-3 py-5">
          <div className="flex min-w-0 items-center gap-3">
            <div className="brand"><Activity size={23} aria-hidden="true" /></div>
            <div className="min-w-0">
              <h1 className="text-xl font-semibold text-zinc-900">Monitor de atividade</h1>
              <p className="mt-0.5 text-xs text-zinc-500">Momor / {location.host} / {store.sessionId.slice(0, 8)}</p>
            </div>
          </div>
          <div className="flex items-center gap-3"><span className="flex items-center gap-2 text-xs text-zinc-500">
            <span className="live-dot" /> {`Verifica\u00e7\u00e3o: ${POLL_INTERVAL} ms`}
          </span><button className="icon-button" onClick={toggleFullscreen} title="Alternar tela cheia" aria-label="Alternar tela cheia"><Maximize2 size={18} /></button></div>
        </div>
      </header>

      <section className={`status-band ${active ? 'status-active' : 'status-away'}`} aria-label="Estado atual">
        <div className="shell flex items-center gap-5 py-7 sm:py-9">
          <StatusIcon className="shrink-0" size={48} strokeWidth={1.6} aria-hidden="true" />
          <div className="min-w-0">
            <p className="text-xs font-semibold uppercase">Estado atual</p>
            <h2 className="mt-1 text-2xl font-semibold" data-testid="status">
              {active ? 'Dentro da p\u00e1gina' : 'Sa\u00edda detectada'}
            </h2>
            <p className="mt-1 text-sm opacity-85">{reason}</p>
            {directReasons(model.current).length > 0 && <p className="mt-2 break-words font-mono text-xs opacity-85">{directReasons(model.current).join(' / ')}</p>}
          </div>
        </div>
      </section>

      <div className="shell py-6">
        <section className="grid grid-cols-2 gap-y-5 border-b border-zinc-200 pb-6 lg:grid-cols-4" aria-label="Sinais do navegador">
          <Metric icon={Eye} label="document.visibilityState" value={model.current?.visibility ?? '-'} />
          <Metric icon={Focus} label="document.hasFocus()" value={String(model.current?.focused ?? false)} />
          <Metric icon={Activity} label="document.hidden" value={String(model.current?.hidden ?? false)} />
          <Metric icon={History} label={'Ciclo da p\u00e1gina'} value={model.current?.lifecycle ?? '-'} />
        </section>

        <section className="grid grid-cols-2 gap-x-5 gap-y-6 border-b border-zinc-200 py-6 md:grid-cols-4" aria-label="Resumo">
          <Summary label={'Sa\u00eddas'} value={String(model.exits)} color="text-red-700" />
          <Summary label="Entradas" value={String(model.entries)} color="text-emerald-700" />
          <Summary label={'Ind\u00edcios registrados'} value={String(readings.hintCount)} color="text-amber-700" />
          <Summary label={'\u00daltima sa\u00edda'} value={model.lastExitAt ? clock.format(model.lastExitAt) : '-'} />
        </section>

        <nav className="view-tabs" aria-label="Diagnosticos">
          {([{ id: 'signals', label: 'Sinais', icon: Activity }, { id: 'frames', label: 'Iframes', icon: Globe },
            { id: 'settings', label: 'Configura\u00e7\u00e3o', icon: Timer }] as const).map(tab =>
            <button key={tab.id} aria-current={view === tab.id ? 'page' : undefined}
              onClick={() => setView(tab.id)}><tab.icon size={15} />{tab.label}</button>)}
        </nav>

        <section hidden={view !== 'signals'} aria-label="Sondas de atividade">
          <div className="probe-grid">
            <Metric icon={Timer} label="Timer / maior atraso" value={`${duration(readings.timerDelayMs)} / ${duration(readings.maxTimerDelayMs)}`} />
            <Metric icon={Activity} label="RAF / maior intervalo" value={`${duration(readings.animationGapMs)} / ${duration(readings.maxAnimationGapMs)}`} />
            <Metric icon={Cpu} label="Worker / entrega" value={readings.workerDelayMs === null ? 'Aguardando' : `${duration(readings.workerDelayMs)} / ${duration(readings.workerDeliveryMs ?? 0)}`} />
            <Metric icon={Cpu} label="Long tasks / maior" value={readings.capabilities.LongTasks ? `${readings.longTaskCount} / ${duration(readings.longestTaskMs)}` : 'Indispon\u00edvel'} />
            <Metric icon={Keyboard} label="Entrada / inatividade" value={`${readings.interactionCount} / ${duration(readings.inactiveMs)}`} />
            <Metric icon={MousePointer2} label={'Ponteiro na p\u00e1gina'} value={readings.pointerInside === null ? 'Sem evento' : String(readings.pointerInside)} />
            <Metric icon={Focus} label="Elemento ativo" value={readings.activeElement} />
            <Metric icon={History} label={'Navega\u00e7\u00e3o / descarte'} value={`${readings.navigationType} / ${readings.wasDiscarded}`} />
            <Metric icon={Globe} label="Servidor / maior intervalo" value={readings.serverIntervalMs === null ? 'Aguardando' : `${duration(readings.serverIntervalMs)} / ${duration(readings.serverMaxGapMs)}`} />
            <Metric icon={Globe} label="Servidor RTT / respostas" value={readings.serverRoundTripMs === null ? 'Aguardando' : `${duration(readings.serverRoundTripMs)} / ${readings.serverCount}`} />
            <Metric icon={Globe} label="Iframes conectados" value={`${readings.frames.filter(frame => frame.status === 'connected').length} / ${readings.frames.length}`} />
            <Metric icon={Eye} label="document.prerendering" value={String(readings.prerendering)} />
          </div>
          <div className="sample-section">
            <div className="mb-2 flex flex-wrap justify-between gap-2 text-xs text-zinc-500"><span>{`\u00daltimas ${readings.samples.length} amostras`}</span><span>RAF: {readings.framesPerSecond} fps / Worker: {readings.workerCount}</span></div>
            <div className="sample-strip" aria-label="Historico de amostras">{readings.samples.map(sample =>
              <span key={sample.at} className={!sample.active ? 'sample-away' : sample.hints ? 'sample-hint' : 'sample-active'}
                title={`${clock.format(sample.at)} / ${sample.active ? 'ativo' : 'ausente'} / indicios=${sample.hints} / timer=${sample.timerDelayMs}ms`} />)}</div>
            <div className="mt-3 flex flex-wrap gap-x-5 gap-y-2 text-xs text-zinc-500"><span>Viewport: {readings.viewport}</span><span>Visual: {readings.visualViewport}</span><span>Tela cheia: {String(readings.fullscreen)}</span><span>Pointer lock: {String(readings.pointerLocked)}</span><span>Online: {String(readings.online)}</span><span>{`Aus\u00eancia conclu\u00edda: ${duration(model.totalAbsenceMs)}`}</span></div>
          </div>
          <div className="hint-section" aria-label="Indicios ativos">
            <div className="flex items-center gap-2 text-sm font-semibold"><TriangleAlert size={16} className="text-amber-700" />{`Ind\u00edcios ativos`}<span className="font-normal text-zinc-500">{readings.alerts.length}</span></div>
            {readings.alerts.length === 0 ? <p className="mt-2 text-xs text-zinc-500">Nenhum</p> :
              <ul className="mt-3 grid gap-2">{readings.alerts.map(alert => <li key={alert.id} className="hint-row"><span className="font-mono text-xs font-semibold">{alert.id}</span><span className="break-words text-xs">{alert.details}</span><time className="text-xs tabular-nums text-zinc-500">{clock.format(alert.at)}</time></li>)}</ul>}
          </div>
        </section>

        <section hidden={view !== 'frames'} className="frame-section" aria-label="Sondas de iframes">
          <FrameProbe id="same-origin" label="Mesma origem" source={sameFrameUrl} reading={readings.frames.find(frame => frame.id === 'same-origin')} />
          <FrameProbe id="cross-origin" label="Outra origem" source={crossFrameUrl} reading={readings.frames.find(frame => frame.id === 'cross-origin')} />
        </section>

        <section hidden={view !== 'settings'} aria-label="Configuracao de sondas" className="settings-section">
          <div className="settings-grid">
            {([{ key: 'timerDelayMs', label: 'Atraso do timer (ms)' }, { key: 'animationGapMs', label: 'Intervalo RAF (ms)' },
              { key: 'workerDelayMs', label: 'Atraso do worker (ms)' }, { key: 'frameTimeoutMs', label: 'Sil\u00eancio do iframe (ms)' },
              { key: 'serverDelayMs', label: 'Atraso do servidor (ms)' },
              { key: 'inactivitySeconds', label: 'Inatividade (s)' }] as const).map(setting =>
              <label key={setting.key} className="text-xs text-zinc-500">{setting.label}<input className="control mt-2" type="number"
                min={settingRanges[setting.key][0]} max={settingRanges[setting.key][1]} step={setting.key === 'inactivitySeconds' ? 1 : 100}
                defaultValue={readings.settings[setting.key]} onBlur={event => {
                  probes.updateSettings({ [setting.key]: Number(event.target.value) });
                  event.target.value = String(probes.getSnapshot().settings[setting.key]);
                }} onKeyDown={event => { if (event.key === 'Enter') event.currentTarget.blur(); }} /></label>)}
          </div>
          <div className="my-5 flex flex-wrap gap-x-6 gap-y-3">
            {([{ key: 'inactivityEnabled', label: 'Ind\u00edcios de inatividade' }, { key: 'elementFocusEnabled', label: 'Foco de elementos nos logs' }] as const).map(setting =>
              <label key={setting.key} className="flex items-center gap-2 text-sm"><input type="checkbox" checked={readings.settings[setting.key]} onChange={event => probes.updateSettings({ [setting.key]: event.target.checked } as Partial<ProbeSettings>)} />{setting.label}</label>)}
          </div>
          <div className="capabilities">{Object.entries(readings.capabilities).map(([name, supported]) => <div key={name}><span>{name}</span><span className={supported ? 'text-emerald-700' : 'text-zinc-500'}>{supported ? 'Dispon\u00edvel' : 'Indispon\u00edvel'}</span></div>)}</div>
          {readings.settingsStorageError && <p role="alert" className="mt-3 text-xs text-red-700">{`Configura\u00e7\u00e3o sem persist\u00eancia`}</p>}
        </section>

        <section className="flex flex-wrap gap-4 border-b border-zinc-200 py-5" aria-label="Controles de teste">
          <div className="flex min-w-0 flex-1 flex-col gap-1.5 text-xs font-medium text-zinc-500">
            <label htmlFor="test-input">Campo de teste</label>
            <input id="test-input" className="control" type="text" placeholder="Texto" autoComplete="off" />
          </div>
          <div className="flex min-w-0 flex-1 flex-col gap-1.5 text-xs font-medium text-zinc-500">
            <label htmlFor="test-select">{`Sele\u00e7\u00e3o`}</label>
            <select id="test-select" className="control" defaultValue="a">
              <option value="a">{`Op\u00e7\u00e3o A`}</option>
              <option value="b">{`Op\u00e7\u00e3o B`}</option>
              <option value="c">{`Op\u00e7\u00e3o C`}</option>
            </select>
          </div>
        </section>

        <section className="pt-6" aria-label="Registro de eventos">
          <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <h2 className="text-base font-semibold text-zinc-900">Registro de eventos</h2>
              <span className="text-xs tabular-nums text-zinc-500">{model.events.length} / {MAX_EVENTS}</span>
            </div>
            <div className="flex items-center gap-1">
              <button className="icon-button" onClick={download} title="Exportar logs JSON" aria-label="Exportar logs JSON"><Download size={18} /></button>
              <button className="icon-button" onClick={downloadCsv} title="Exportar logs CSV" aria-label="Exportar logs CSV"><FileSpreadsheet size={18} /></button>
              <button className="icon-button" onClick={() => { probes.clear(); store.clear(); setPage(0); }} title="Limpar logs" aria-label="Limpar logs"><Trash2 size={18} /></button>
            </div>
          </div>
          <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
            <div className="segments" role="group" aria-label="Filtrar eventos">
              {(['todos', 'saida', 'entrada', 'indicio'] as const).map(item => (
                <button key={item} onClick={() => { setFilter(item); setPage(0); }} aria-pressed={filter === item}
                  className={filter === item ? 'selected' : ''}>
                  {item === 'todos' ? 'Todos' : item === 'saida' ? 'Sa\u00eddas' : item === 'entrada' ? 'Entradas' : 'Ind\u00edcios'}
                </button>
              ))}
            </div>
            <label className="search-control">
              <Search size={16} aria-hidden="true" />
              <input value={query} onChange={event => { setQuery(event.target.value); setPage(0); }} aria-label="Buscar eventos" placeholder="Buscar eventos" />
            </label>
          </div>
          {store.hasStorageError() && <p role="alert" className="mb-3 text-sm text-red-700">{`Armazenamento indispon\u00edvel. Logs apenas nesta sess\u00e3o.`}</p>}
          <div className="log-scroll">
            <table>
              <thead><tr><th>{'Hor\u00e1rio'}</th><th>Movimento</th><th>{'Evid\u00eancia'}</th><th>Evento / contexto</th><th>Visibilidade / foco</th><th className="native-column">Origem</th><th>{'Aus\u00eancia'}</th></tr></thead>
              <tbody>{visibleRows.map(event => {
                const RowIcon = event.direction === 'saida' ? ArrowUpRight : event.direction === 'entrada' ? ArrowDownLeft : Check;
                return <tr key={event.id} data-direction={event.direction} data-source={event.source} data-scope={event.scope} data-evidence={event.evidence}>
                  <td><time dateTime={new Date(event.at).toISOString()} className="tabular-nums">{clock.format(event.at)}</time><span className="date-label">{date.format(event.at)}</span></td>
                  <td><span className={`direction direction-${event.direction}`}><RowIcon size={14} />{labels[event.direction]}</span></td>
                  <td className={`evidence evidence-${event.evidence}`}>{evidenceLabels[event.evidence]}</td>
                  <td className="event-cell"><span className="font-mono text-xs">{event.source}</span><span className="event-scope">{event.scope}</span>{event.details && <span className="event-details">{event.details}</span>}</td>
                  <td className="font-mono text-xs">{event.visibility} / {String(event.focused)}</td>
                  <td className="native-column text-xs text-zinc-500">{event.trusted === null ? 'Leitura' : event.trusted ? 'Nativo' : 'Sint\u00e9tico'}</td>
                  <td className="text-xs tabular-nums">{event.absenceMs === null ? '-' : duration(event.absenceMs)}</td>
                </tr>;
              })}</tbody>
            </table>
            {rows.length === 0 && <p className="py-10 text-center text-sm text-zinc-500">Nenhum evento</p>}
          </div>
          <div className="mt-3 flex items-center justify-between text-xs text-zinc-500"><span>{rows.length} eventos</span><div className="flex items-center gap-2"><button className="icon-button" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)} title="Pagina anterior" aria-label="Pagina anterior"><ChevronLeft size={16} /></button><span>{currentPage + 1} / {lastPage + 1}</span><button className="icon-button" disabled={currentPage >= lastPage} onClick={() => setPage(currentPage + 1)} title="Proxima pagina" aria-label="Proxima pagina"><ChevronRight size={16} /></button></div></div>
        </section>
        <footer className="mt-5 flex flex-wrap justify-between gap-2 text-xs text-zinc-500">
          <span>{store.hasStorageError() ? 'Somente nesta janela' : 'Logs salvos neste navegador'}</span>
          <span>{`\u00daltimo evento: ${model.events[0]?.source ?? '-'}`}</span>
        </footer>
      </div>
    </main>
  );
}

function FrameProbe({ id, label, source, reading }: {
  id: string; label: string; source: string; reading?: FrameReading;
}) {
  const reference = useRef<HTMLIFrameElement>(null);
  useEffect(() => {
    if (reference.current) return probes.registerFrame(reference.current, id);
  }, [id]);
  return <div className="frame-tool"><div className="mb-3 flex items-center justify-between gap-3"><h3 className="text-sm font-semibold">{label}</h3><span className={`frame-connection connection-${reading?.status ?? 'waiting'}`}>{reading?.status === 'connected' ? 'Conectado' : reading?.status === 'late' ? 'Sem heartbeat' : 'Aguardando'}</span></div>
    <iframe ref={reference} src={source} title={`Sonda ${label}`} />
    <dl className="frame-readings"><div><dt>Visibilidade</dt><dd>{reading?.sample?.visibility ?? '-'}</dd></div><div><dt>hasFocus()</dt><dd>{String(reading?.sample?.focused ?? false)}</dd></div><div><dt>Heartbeat</dt><dd>{reading?.heartbeatCount ?? 0}</dd></div><div><dt>{'\u00daltima leitura'}</dt><dd>{reading?.receivedAt ? clock.format(reading.receivedAt) : '-'}</dd></div></dl>
  </div>;
}

function Metric({ icon: Icon, label, value }: {
  icon: typeof Eye; label: string; value: string;
}) {
  return <div className="min-w-0 pr-3"><div className="flex items-center gap-2 text-zinc-500"><Icon size={15} className="shrink-0" /><span className="break-words font-mono text-xs">{label}</span></div><p className="mt-2 text-base font-semibold text-zinc-900">{value}</p></div>;
}
function Summary({ label, value, color = 'text-zinc-900' }: {
  label: string; value: string; color?: string;
}) {
  return <div className="min-w-0"><p className="text-xs text-zinc-500">{label}</p><p className={`mt-1.5 break-words text-xl font-semibold tabular-nums ${color}`}>{value}</p></div>;
}

const root = document.getElementById('root');
if (!root) throw new Error('Missing application root');
createRoot(root).render(<App />);
