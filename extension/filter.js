"use strict";
//
// The capture decision, as pure functions over plain values, so it can be
// exercised by `filter.test.mjs` under node without a browser.
//

/// Only http(s) is capturable. This is what rejects blob:, data:, file:,
/// chrome-extension: and friends, which have no bytes for us to fetch.
export function httpUrl(raw) {
  let u;
  try {
    u = new URL(raw);
  } catch (_) {
    return null;
  }
  return u.protocol === "http:" || u.protocol === "https:" ? u : null;
}

/// Lowercase, dotless. Applied to both the host's list and the candidate.
export function normalizeExtension(value) {
  return String(value).toLowerCase().replace(/^\./, "");
}

export function normalizeHost(value) {
  return String(value).toLowerCase().replace(/^\./, "");
}

export function extensionOf(url, filename) {
  const name = (filename || url.pathname).split(/[?#]/)[0];
  const m = /\.([A-Za-z0-9]{1,12})$/.exec(name);
  return m ? m[1].toLowerCase() : "";
}

/// Suffix match, so `example.com` also excludes `cdn.example.com`.
export function hostExcepted(url, exceptionHosts) {
  const h = url.hostname.toLowerCase();
  return exceptionHosts.some((e) => h === e || h.endsWith("." + e));
}

/// An empty capture list captures nothing: the safe default when the host has
/// not told us what it wants.
export function shouldCapture(url, filename, lists) {
  if (!lists || !lists.captureExtensions || !lists.captureExtensions.length) return false;
  if (hostExcepted(url, lists.exceptionHosts || [])) return false;
  if (lists.captureExtensions.includes("*")) return true;
  return lists.captureExtensions.includes(extensionOf(url, filename));
}

/// Firefox exposes the promise-based `browser`; Chromium only has `chrome`
/// (promise-based in MV3 too). Prefer `browser` when both exist.
export function pickApi(scope) {
  return scope.browser ?? scope.chrome;
}

/// Resolves to whether the user allowed this extension in Incognito / private
/// windows. Chromium MV3 returns a promise; a callback form is the fallback for
/// builds that do not. Missing namespace or any error means "not allowed".
export function checkIncognitoAccess(ext) {
  if (!ext || typeof ext.isAllowedIncognitoAccess !== "function") return Promise.resolve(false);
  return new Promise((resolve) => {
    let r;
    try {
      r = ext.isAllowedIncognitoAccess(); // promise form; Firefox takes no callback
    } catch (_) {
      r = undefined;
    }
    if (r && typeof r.then === "function") return r.then((v) => resolve(v === true), () => resolve(false));
    try {
      ext.isAllowedIncognitoAccess((v) => resolve(v === true));
    } catch (_) {
      resolve(false);
    }
  });
}

/// Firefox reports `filename` as the full target path, Chromium as a bare name.
/// The host wants a name, and its sanitiser would otherwise glue the
/// directories into it.
export function basename(path) {
  return String(path || "").split(/[\\/]/).pop();
}
