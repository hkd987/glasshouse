// Added by the app bridge to every artifact page (ADR 0033): tells the
// Artifacts app where a person clicked to pin a comment, and where pinned
// elements are now. It runs inside the artifact's sandbox, so the page can
// change what it says; that misplaces only its own pins. The app never
// sends comments in, only element paths.
(() => {
  "use strict";
  const TAG = "artifact";
  const MAX_PATH = 512;
  const MAX_PATHS = 200;
  let pinning = false;

  const send = (message) => {
    try {
      window.parent.postMessage({ glass: TAG, ...message }, "*");
    } catch (_) {
      // Not framed: nothing to tell.
    }
  };

  // The element's path from the body: tag:nth-of-type(n) steps.
  const pathOf = (element) => {
    const steps = [];
    let node = element;
    while (node && node.nodeType === 1 && node !== document.body && node !== document.documentElement) {
      let n = 1;
      for (let sibling = node.previousElementSibling; sibling; sibling = sibling.previousElementSibling) {
        if (sibling.localName === node.localName) n += 1;
      }
      steps.unshift(`${node.localName}:nth-of-type(${n})`);
      node = node.parentElement;
    }
    const path = steps.join(" > ");
    return path.length > 0 && path.length <= MAX_PATH ? path : null;
  };

  const thousandths = (value, size) => Math.max(0, Math.min(1000, Math.round((value / Math.max(1, size)) * 1000)));

  const where = (paths) =>
    paths.map((path) => {
      try {
        const element = document.body.querySelector(`:scope > ${path}`);
        if (!element) return null;
        const r = element.getBoundingClientRect();
        return { path, x: r.left, y: r.top, w: r.width, h: r.height };
      } catch (_) {
        return null;
      }
    });

  let asked = [];
  let queued = false;
  const tell = () => {
    queued = false;
    if (asked.length > 0) send({ type: "rects", rects: where(asked) });
  };
  const later = () => {
    if (!queued) {
      queued = true;
      requestAnimationFrame(tell);
    }
  };

  window.addEventListener("message", (event) => {
    if (event.source !== window.parent) return;
    const data = event.data;
    if (!data || data.glass !== TAG) return;
    if (data.type === "pin") {
      pinning = data.on === true;
      document.documentElement.style.cursor = pinning ? "crosshair" : "";
    } else if (data.type === "where" && Array.isArray(data.paths)) {
      asked = data.paths.filter((p) => typeof p === "string" && p.length <= MAX_PATH).slice(0, MAX_PATHS);
      tell();
    }
  });

  window.addEventListener(
    "click",
    (event) => {
      if (!pinning) return;
      event.preventDefault();
      event.stopPropagation();
      const doc = document.documentElement;
      send({
        type: "pick",
        path: event.target instanceof Element ? pathOf(event.target) : null,
        x: thousandths(event.pageX, doc.scrollWidth),
        y: thousandths(event.pageY, doc.scrollHeight),
      });
    },
    true,
  );

  window.addEventListener("scroll", later, { passive: true });
  window.addEventListener("resize", later);
  const ready = () => send({ type: "ready", height: document.documentElement.scrollHeight });
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", ready);
  } else {
    ready();
  }
})();
