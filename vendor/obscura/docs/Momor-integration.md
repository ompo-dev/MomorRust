# Momor integration

Momor uses Obscura as a local browser engine. The two projects stay separate:
Momor owns the desktop surface and Obscura owns V8, networking, DOM, layout,
painting, cookies, and CDP.

## Engine build

The Momor browser surface needs an Obscura binary built with native rendering:

```bash
cargo build -p obscura-cli --bin obscura --features render
```

Set `MOMOR_OBSCURA_BINARY` to that executable when it is not available on
`PATH`. Momor first tries the endpoint in `MOMOR_OBSCURA_CDP_URL`; when that
endpoint is unavailable it starts the configured binary itself.

## Startup contract

Momor starts one local, single-worker server with a command equivalent to:

```text
obscura serve --host 127.0.0.1 --port 0 --workers 1 \
  --max-connections 4 --ready-file <temporary-file>
```

The `ready-file` is atomically published only after V8 initialization and
contains the bound address:

```json
{
  "pid": 1234,
  "host": "127.0.0.1",
  "port": 43127,
  "websocket_path": "/devtools/browser"
}
```

Momor builds the WebSocket endpoint from this record, connects over CDP, and
terminates the child process when the browser session is released. The ready
file is private and is removed during cleanup.

## First vertical slice

The desktop surface currently uses these commands on one browser target:

1. `Target.createTarget`
2. `Target.attachToTarget`
3. `Page.enable` and `Runtime.enable`
4. `Page.navigate`
5. `Page.captureScreenshot`

The screenshot is a transport surface while the integration is being brought
up. It is intentionally not treated as the final compositor architecture.

## Security boundary

The default server binds to loopback. `file://` navigation remains disabled,
and Momor only accepts `http://`, `https://`, and `about:` address-bar URLs in
this first slice. Remote CDP exposure requires Obscura's bearer token policy;
Momor does not weaken that policy.

## Next protocol steps

The next integration stages should preserve this boundary:

* replace screenshot polling with `Page.startScreencast` frames;
* map GPUI pointer and keyboard events to `Input.dispatchMouseEvent` and
  `Input.dispatchKeyEvent`;
* add target lifecycle events for multiple Momor browser tabs;
* add navigation history, downloads, permissions, cookies, and profile
  storage behind explicit user-visible controls;
* add a CDP compatibility smoke test that runs against the rendered Obscura
  binary from the Momor repository's integration harness.

