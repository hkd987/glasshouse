// The app's service worker: its own files, network first, the last copy
// when there's no connection (UI-5: apps work offline). Bridge calls are
// never cached here; the client keeps reads itself (cache.ts).

/// <reference lib="webworker" />
export {};
declare const self: ServiceWorkerGlobalScope;

const CACHE = "glass-app-files-v1";
const FILES = ["./", "./index.html", "./app.js", "./app.css"];

self.addEventListener("install", (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(FILES))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (
    event.request.method !== "GET" ||
    url.origin !== self.location.origin ||
    url.pathname.startsWith("/_bridge/")
  ) {
    return;
  }
  event.respondWith(
    fetch(event.request)
      .then((response) => {
        if (response.ok) {
          const copy = response.clone();
          void caches.open(CACHE).then((cache) => cache.put(event.request, copy));
        }
        return response;
      })
      .catch(async () => {
        const cached =
          (await caches.match(event.request, { ignoreSearch: true })) ??
          (event.request.mode === "navigate" ? await caches.match("./index.html") : undefined);
        return cached ?? Response.error();
      }),
  );
});
