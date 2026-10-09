let sequence = 0;
let previous = performance.now();
setInterval(() => {
  const at = performance.now();
  self.postMessage({ sequence: ++sequence, epochAt: performance.timeOrigin + at, intervalMs: at - previous });
  previous = at;
}, 1000);
