// Self-check for the capture decision. Run: node extension/filter.test.mjs
// Silence plus exit code 0 means pass.
import assert from "node:assert/strict";
import { extensionOf, hostExcepted, httpUrl, normalizeExtension, shouldCapture } from "./filter.js";

const lists = { captureExtensions: ["zip", "exe"], exceptionHosts: ["example.com"] };

// Scheme gate
assert.equal(httpUrl("blob:https://e.test/abc"), null);
assert.equal(httpUrl("data:text/plain,hi"), null);
assert.equal(httpUrl("file:///C:/x.zip"), null);
assert.equal(httpUrl("chrome-extension://abc/x.zip"), null);
assert.equal(httpUrl("ftp://e.test/x.zip"), null);
assert.equal(httpUrl("not a url"), null);
assert.equal(httpUrl("https://e.test/x.zip").hostname, "e.test");

// Extension extraction ignores query and fragment, prefers the suggested name
assert.equal(extensionOf(httpUrl("https://e.test/d/a.ZIP?k=v#f")), "zip");
assert.equal(extensionOf(httpUrl("https://e.test/dir/")), "");
assert.equal(extensionOf(httpUrl("https://e.test/get?id=1"), "setup.exe"), "exe");
assert.equal(extensionOf(httpUrl("https://e.test/a.verylongextension")), "");
assert.equal(normalizeExtension(".ZIP"), "zip");

// Exception hosts match the host and its subdomains, not a lookalike suffix
assert.equal(hostExcepted(httpUrl("https://example.com/a.zip"), ["example.com"]), true);
assert.equal(hostExcepted(httpUrl("https://cdn.example.com/a.zip"), ["example.com"]), true);
assert.equal(hostExcepted(httpUrl("https://notexample.com/a.zip"), ["example.com"]), false);
assert.equal(hostExcepted(httpUrl("https://EXAMPLE.com/a.zip"), ["example.com"]), true);

// Whole rule
assert.equal(shouldCapture(httpUrl("https://e.test/a.zip"), "", lists), true);
assert.equal(shouldCapture(httpUrl("https://e.test/a.txt"), "", lists), false);
assert.equal(shouldCapture(httpUrl("https://cdn.example.com/a.zip"), "", lists), false);
assert.equal(shouldCapture(httpUrl("https://e.test/get"), "a.zip", lists), true);

// No list from the host => capture nothing, rather than everything
assert.equal(shouldCapture(httpUrl("https://e.test/a.zip"), "", { captureExtensions: [], exceptionHosts: [] }), false);
assert.equal(shouldCapture(httpUrl("https://e.test/a.zip"), "", null), false);

// Browser-API shim and filename normalisation
import { basename, pickApi } from "./filter.js";
const chromeNs = { id: "chrome" }, browserNs = { id: "browser" };
assert.equal(pickApi({ chrome: chromeNs }), chromeNs);
assert.equal(pickApi({ chrome: chromeNs, browser: browserNs }), browserNs);
assert.equal(pickApi({}), undefined);
assert.equal(basename("C:\\Users\\me\\Downloads\\a.zip"), "a.zip");
assert.equal(basename("/home/me/Downloads/a.zip"), "a.zip");
assert.equal(basename("a.zip"), "a.zip");
assert.equal(basename(undefined), "");

// Incognito access: promise form, callback fallback, missing namespace
import { checkIncognitoAccess } from "./filter.js";
assert.equal(await checkIncognitoAccess(undefined), false);
assert.equal(await checkIncognitoAccess({}), false);
assert.equal(await checkIncognitoAccess({ isAllowedIncognitoAccess: () => Promise.resolve(true) }), true);
assert.equal(await checkIncognitoAccess({ isAllowedIncognitoAccess: () => Promise.resolve(false) }), false);
assert.equal(await checkIncognitoAccess({ isAllowedIncognitoAccess: () => Promise.reject(new Error("x")) }), false);
assert.equal(await checkIncognitoAccess({ isAllowedIncognitoAccess: (cb) => cb(true) }), true);
assert.equal(await checkIncognitoAccess({ isAllowedIncognitoAccess: () => { throw new Error("x"); } }), false);
