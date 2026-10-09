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

The most recent 500 events and aggregate counts are stored in this origin's
localStorage. Reload starts a new live observation without discarding past logs.
Separate tabs have independent in-memory monitors; the latest persistence write
wins for this origin. The JSON export includes the current readings and event log.

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
The synthetic lifecycle check is explicitly logged as synthetic; navigation
also verifies a native pagehide event. Generated screenshots stay in `artifacts/`.

This diagnostic only observes web APIs. It cannot verify whether external
software, extensions, screen capture or server-side signals detect activity.
