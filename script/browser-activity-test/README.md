# Browser activity test

Local React, TypeScript and Tailwind diagnostic page for the Momor browser.

```powershell
npm.cmd ci
npm.cmd run dev
```

Open http://127.0.0.1:5173 in Momor. Green means the browser reports a visible,
focused and running document without an outstanding window blur event. Red means
at least one of those signals is absent or a window blur event was observed.
The page does not emulate focus or visibility and does not change browser policy.

Window focus/blur, visibility, page lifecycle events and changed readings sampled
every 250 ms are recorded. Element focus changes and pointer movement are not
treated as departures. Browser event provenance (`isTrusted`) is recorded too.
Timer throttling can delay sampled readings when the page is backgrounded.

The most recent 2,000 events and aggregate counts are stored in localStorage,
under a per-tab namespace identified through sessionStorage. Reload starts a new
live observation without discarding past logs. Independently opened tabs do not
overwrite each other's histories. When BroadcastChannel is available, an initial
ownership handshake detects duplicated live tab identities and forks their
namespace before writing. Without that API, use independently opened tabs.
Legacy v1 logs migrate once without deleting the original copy.

## Evidence

- Direct: visibility, document/window focus, lifecycle, active-frame blur.
- Hints only: pointer exit, lack of trusted input, timer/RAF/worker delays,
  long tasks, missed iframe heartbeats and local server heartbeat gaps.
- Diagnostics: element focus, navigation type, bfcache `persisted`, discard,
  prerendering, viewport, fullscreen, pointer lock, connectivity and clipboard
  event names. Clipboard contents and typed text are never collected.

Hints never turn the page red or increment its departure counter. Timing gaps
can result from CPU load, sleep, browser throttling or network problems, not
necessarily a tab/app switch. Ordinary movement between input fields and iframe
focus is distinguished from a page departure. Actual iframe blur is still
recorded even when the parent document keeps reporting focus.

Timers use monotonic clocks for durations. Exports include wall and monotonic
timestamps, event provenance, scope, details, current readings and settings.
JSON and CSV exports cover all retained logs; the table is paginated. Thresholds
are editable and persist with the tab. APIs that are unavailable are identified.

## Independent probes

The monitor collects main-thread timer drift, animation gaps/FPS, dedicated
worker timer and delivery latency, and Long Tasks API entries where supported.
Two cooperative test iframes (127.0.0.1 and localhost) independently report their
signals. Their messages must match the registered WindowProxy, origin, token,
frame id, schema and increasing sequence before they are accepted. The fixture
does not read DOM inside arbitrary third-party iframes.

The `/api/activity-heartbeat` endpoint exists in Vite dev/preview only. A ping
every two seconds measures server-side intervals with a monotonic Node clock.
It accepts local same-port origins, JSON POST requests and bounded opaque ids.
It retains only bounded in-memory session timing counters, not browser content.
No telemetry is transmitted to an external service. Static hosting without this
endpoint reports that probe as unavailable.

```powershell
npm.cmd test
npm.cmd run build
```

Browser checks and desktop/mobile screenshots:

```powershell
npx.cmd playwright install chromium
npm.cmd run test:e2e
```

Alternatively, use an already installed Chromium-based browser without a
download, for example `$env:ACTIVITY_TEST_BROWSER_CHANNEL = 'msedge'`.
Browser tests own an isolated context and never connect to existing user tabs.
Synthetic signal checks are explicitly logged as synthetic. Real navigation,
iframe focus transfer, bounded main-thread blocking, isolated-tab persistence,
message rejection, exports and heartbeat failures are checked independently.
Generated screenshots stay in `artifacts/`.

This diagnostic only observes web APIs. It cannot verify whether external
software, extensions, screen capture or server-side signals detect activity.
Green means no direct departure signal was observed, not that every possible
external detector was defeated. Unsupported or suppressed APIs, process kills,
lost network packets and browser interventions can leave gaps in observation.
