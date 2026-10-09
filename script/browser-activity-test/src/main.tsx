import { useState, useSyncExternalStore } from 'react';
import { createRoot } from 'react-dom/client';
import { Activity, ArrowDownLeft, ArrowUpRight, Check, Download, Eye, Focus,
  History, Search, ShieldCheck, ShieldX, Trash2 } from 'lucide-react';
import type { Direction } from './activity';
import { createActivityStore, isActive, POLL_INTERVAL } from './activity';
import './style.css';

const store = createActivityStore();
if (import.meta.hot) import.meta.hot.dispose(() => store.dispose());

const clock = new Intl.DateTimeFormat('pt-BR', {
  hour: '2-digit', minute: '2-digit', second: '2-digit', fractionalSecondDigits: 3,
});
const date = new Intl.DateTimeFormat('pt-BR', { day: '2-digit', month: '2-digit' });
const labels = { entrada: 'Entrada', saida: 'Sa\u00edda', evento: 'Evento' };
function duration(ms: number) {
  return ms < 1000 ? `${ms} ms` : `${(ms / 1000).toFixed(1)} s`;
}

function App() {
  const model = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const [filter, setFilter] = useState<Direction | 'todos'>('todos');
  const [query, setQuery] = useState('');
  const active = isActive(model.current);
  const rows = model.events.filter(event => (filter === 'todos' || filter === event.direction)
    && `${event.source} ${labels[event.direction]}`.toLowerCase().includes(query.toLowerCase()));
  const StatusIcon = active ? ShieldCheck : ShieldX;
  const reason = model.current?.lifecycle !== 'running' ? 'Ciclo da p\u00e1gina interrompido'
    : model.current?.hidden || model.current?.visibility !== 'visible' ? 'P\u00e1gina oculta'
    : model.current?.windowBlurred ? 'Evento blur observado'
    : !model.current?.focused ? 'Janela sem foco' : 'Vis\u00edvel e com foco';

  function download() {
    const blob = new Blob([JSON.stringify({ version: 1, exportedAt: new Date().toISOString(),
      url: location.href, userAgent: navigator.userAgent, ...model }, null, 2)],
    { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement('a');
    link.href = url;
    link.download = `momor-atividade-${new Date().toISOString().replace(/[:.]/g, '-')}.json`;
    link.click();
    window.setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  return (
    <main data-status={active ? 'active' : 'away'}>
      <header className="border-b border-zinc-200 bg-white">
        <div className="shell flex flex-wrap items-center justify-between gap-3 py-5">
          <div className="flex min-w-0 items-center gap-3">
            <div className="brand"><Activity size={23} aria-hidden="true" /></div>
            <div className="min-w-0">
              <h1 className="text-xl font-semibold text-zinc-900">Monitor de atividade</h1>
              <p className="mt-0.5 text-xs text-zinc-500">Momor / {location.host}</p>
            </div>
          </div>
          <span className="flex items-center gap-2 text-xs text-zinc-500">
            <span className="live-dot" /> {`Verifica\u00e7\u00e3o: ${POLL_INTERVAL} ms`}
          </span>
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
          <Summary label={'Tempo ausente conclu\u00eddo'} value={duration(model.totalAbsenceMs)} />
          <Summary label={'\u00daltima sa\u00edda'} value={model.lastExitAt ? clock.format(model.lastExitAt) : '-'} />
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
              <span className="text-xs tabular-nums text-zinc-500">{model.events.length} / 500</span>
            </div>
            <div className="flex items-center gap-1">
              <button className="icon-button" onClick={download} title="Exportar logs JSON" aria-label="Exportar logs JSON"><Download size={18} /></button>
              <button className="icon-button" onClick={store.clear} title="Limpar logs" aria-label="Limpar logs"><Trash2 size={18} /></button>
            </div>
          </div>
          <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
            <div className="segments" role="group" aria-label="Filtrar eventos">
              {(['todos', 'saida', 'entrada'] as const).map(item => (
                <button key={item} onClick={() => setFilter(item)} aria-pressed={filter === item}
                  className={filter === item ? 'selected' : ''}>
                  {item === 'todos' ? 'Todos' : item === 'saida' ? 'Sa\u00eddas' : 'Entradas'}
                </button>
              ))}
            </div>
            <label className="search-control">
              <Search size={16} aria-hidden="true" />
              <input value={query} onChange={event => setQuery(event.target.value)} aria-label="Buscar eventos" placeholder="Buscar eventos" />
            </label>
          </div>
          {store.hasStorageError() && <p role="alert" className="mb-3 text-sm text-red-700">{`Armazenamento indispon\u00edvel. Logs apenas nesta sess\u00e3o.`}</p>}
          <div className="log-scroll">
            <table>
              <thead><tr><th>{'Hor\u00e1rio'}</th><th>Movimento</th><th>Evento</th><th>Visibilidade / foco</th><th className="native-column">Origem</th><th>{'Aus\u00eancia'}</th></tr></thead>
              <tbody>{rows.map(event => {
                const RowIcon = event.direction === 'saida' ? ArrowUpRight : event.direction === 'entrada' ? ArrowDownLeft : Check;
                return <tr key={event.id} data-direction={event.direction}>
                  <td><time dateTime={new Date(event.at).toISOString()} className="tabular-nums">{clock.format(event.at)}</time><span className="date-label">{date.format(event.at)}</span></td>
                  <td><span className={`direction direction-${event.direction}`}><RowIcon size={14} />{labels[event.direction]}</span></td>
                  <td className="font-mono text-xs">{event.source}</td>
                  <td className="font-mono text-xs">{event.visibility} / {String(event.focused)}</td>
                  <td className="native-column text-xs text-zinc-500">{event.trusted === null ? 'Leitura' : event.trusted ? 'Nativo' : 'Sint\u00e9tico'}</td>
                  <td className="text-xs tabular-nums">{event.absenceMs === null ? '-' : duration(event.absenceMs)}</td>
                </tr>;
              })}</tbody>
            </table>
            {rows.length === 0 && <p className="py-10 text-center text-sm text-zinc-500">Nenhum evento</p>}
          </div>
        </section>
        <footer className="mt-5 flex flex-wrap justify-between gap-2 text-xs text-zinc-500">
          <span>{store.hasStorageError() ? 'Somente nesta janela' : 'Logs salvos neste navegador'}</span>
          <span>{`\u00daltimo evento: ${model.events[0]?.source ?? '-'}`}</span>
        </footer>
      </div>
    </main>
  );
}

function Metric({ icon: Icon, label, value }: {
  icon: typeof Eye; label: string; value: string;
}) {
  return <div className="min-w-0 pr-3"><div className="flex items-center gap-2 text-zinc-500"><Icon size={15} className="shrink-0" /><span className="break-all font-mono text-xs">{label}</span></div><p className="mt-2 text-base font-semibold text-zinc-900">{value}</p></div>;
}
function Summary({ label, value, color = 'text-zinc-900' }: {
  label: string; value: string; color?: string;
}) {
  return <div className="min-w-0"><p className="text-xs text-zinc-500">{label}</p><p className={`mt-1.5 break-words text-xl font-semibold tabular-nums ${color}`}>{value}</p></div>;
}

const root = document.getElementById('root');
if (!root) throw new Error('Missing application root');
createRoot(root).render(<App />);
