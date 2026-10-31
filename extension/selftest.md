# Manual self-check

Each step lists what to do and the single observable result that means it
passed. Run them in order; later steps assume the earlier ones passed.

Open two windows first: the options page (**Details → Extension options**) and
the service-worker console (**Details → Inspect views: service worker**).

## A. Host absent

1. **Unregister the host** (delete the registry key) and reload the extension.
   Expect: options page shows a **red dot**, `Last error` reads something like
   *Specified native messaging host not found*.
2. Download any file matching the capture list.
   Expect: the file downloads **normally in the browser**, with no stuck
   "Paused" entry in `chrome://downloads`. The console shows no repeating
   reconnect spam.
3. Press **Reconnect** five times in a row.
   Expect: five single attempts, never a loop. The status line shows
   *retrying in Ns* with N growing (2, 4, 8, 16…) between automatic attempts.

## B. Handshake

4. Register the host (run the TDM app once, or `tdm --register-host`),
   restart the browser, press **Reconnect**.
   Expect: **green dot**, `Host version` filled in, and `Capture types` /
   `Excluded hosts` listing exactly what the host sent in `ready`.
5. In the host's own log, confirm one `hello` arrived with `version: "1.0"`.
   Expect: exactly one `hello` per connection, not one per download.

## C. Capture — accepted

6. Make sure the host's `captureExtensions` contains `zip`, then download a
   `.zip` over https.
   Expect: in the host, one `download` message with `url`, `referer`,
   `userAgent`, `filename` and `size` (`size` absent when the server sent no
   `Content-Length`). In the browser, **no file in the download folder** and
   **no entry** in `chrome://downloads` — it is cancelled and erased.
7. Watch the download shelf/bubble during step 6.
   Expect: at most a brief flash. It must not sit in "Paused" state for more
   than the host's reply time.

## D. Capture — declined

8. Have the host reply `declined` to the next `download`, then download a
   `.zip` again.
   Expect: the browser finishes the download itself, the file lands in the
   download folder at the normal suggested name, and the options page's
   `Last download` line reads `declined: <url>`.

## E. Filters

9. Download a file whose extension is **not** in `captureExtensions` (e.g.
   `.txt` when the list is `zip, exe`).
   Expect: downloads normally, and **no `download` message reaches the host**
   (this is the cached-list pre-filter; it should not even connect).
10. Put the test site's host in `exceptionHosts`, reconnect, download a `.zip`
    from it, then from a different host.
    Expect: the excluded host downloads in the browser, the other host is taken
    over. A subdomain of the excluded host is also left alone.
11. Have the host send an **empty** `captureExtensions`, reconnect, download a
    `.zip`.
    Expect: the browser keeps it. Empty means capture nothing.

## F. URL schemes

12. On any page, save an image via a `blob:` or `data:` URL (e.g. a generated
    canvas download), and use Ctrl+S on a local `file:` page.
    Expect: no `download` message for any of them; all handled by the browser.

## G. Context menu

13. Right-click a link → **Download with TDM**.
    Expect: a `download` message with that `url` and the page's `referer`,
    even if the extension is not in the capture list and even with automatic
    capture switched **off**.
14. Right-click an image/video → **Download media with TDM**.
    Expect: the same, with the media `srcUrl`. For video/audio whose URL ends
    in a media extension, `filename` is the page title plus that extension.
14b. Select a paragraph containing three links, right-click →
    **Download selected links with TDM**.
    Expect: three `download` messages in page order, each with the page `referer`.

## H. Toggle

15. Untick **Capture downloads automatically**, download a `.zip`.
    Expect: browser downloads it; nothing reaches the host. Tick it again and
    repeat: taken over.

## I. Host crash mid-flight

16. Kill the host process while a `.zip` download is waiting for a verdict
    (easiest: make the host sleep, start a download, then kill it).
    Expect: within 8 s the browser **resumes** the download and completes it.
    Options page goes red with `Last error` set. No paused leftovers.
17. Make the host accept the connection but never reply.
    Expect: same as 16 — the 8 s timeout fires, the download resumes, and the
    connection is dropped (so the request/reply queue cannot desynchronise).

## J. Service-worker teardown

18. Click **Terminate** next to the service worker in `chrome://extensions`
    (Developer mode), then download a `.zip`.
    Expect: the worker restarts, reconnects, and the download is captured.
    Capture lists survive (they are in `storage.session`), the toggle survives
    a full browser restart (it is in `storage.local`).
19. Download the same URL twice quickly.
    Expect: two separate `download` messages (two download ids), but never two
    messages for the *same* download id.

## K. Cookies (optional permission)

20. Before granting: download a `.zip`.
    Expect: the `download` message has **no** `cookie` field.
21. Press **Allow cookies**, accept the browser prompt, download a
    `.zip` from a site you are logged into.
    Expect: `cookie` present as `name=value; name=value`, and the button is
    replaced by *Allowed*.

## L. Popup

22. Click the toolbar icon on a page with a plain `<video src="https://….mp4">`.
    Expect: status dot (pulsing green when connected), the auto-capture switch
    in sync with the options page, and the video listed as **VIDEO** named
    after the page title. **Download** turns into *Sent ✓* and the app gets a
    `download` with `referer` = the page URL.
23. Open the popup on `chrome://extensions` (or `about:addons`).
    Expect: *TDM can't look inside this page.* and no errors.
24. Stop the app and press **Download** again.
    Expect: the button reads *App offline*; the popup shows **Reconnect**.

## M. Media detection (optional permission)

25. Before granting: hover a video.
    Expect: no pill, and no TDM entry under DevTools → Sources → Content scripts.
26. Options → **Grant media detection**, accept, reload a page with an HLS
    player (`blob:` video).
    Expect: hovering the player shows **⬇ TDM** top-right; the popup lists
    the `.m3u8` as **HLS**. `.ts`/`.m4s` segments are never listed.
27. Click **⬇ TDM**.
    Expect: *✓ Sent to TDM*; the app receives the manifest URL with the
    frame URL as `referer`.
28. Click **×** on the pill, move away and back.
    Expect: the pill stays hidden for that video until reload.
29. Keyboard: Tab to a player with `controls`, then press Tab again.
    Expect: focus moves into the pill (orange focus ring); Esc returns focus
    to the player; Tab from **×** leaves the pill and the next Tab moves on.
30. A page with hostile CSS (`div, button { all: unset !important }`) or a
    strict `style-src` / Trusted Types CSP.
    Expect: the pill looks the same.
31. Switch the OS between dark and light theme.
    Expect: popup, options page and pill all follow.
32. Options → **Turn off**.
    Expect: the pill is gone after a reload; if cookies were allowed, sending
    cookies still works.

## N. Firefox

33. `node extension/build.mjs`, then load `dist/firefox/manifest.json` from
    `about:debugging`, with the host registered for `tdm@tdm.app`.
    Expect: the popup shows connected.
34. Repeat C (accepted) and D (declined).
    Expect: accepted downloads disappear from the Downloads panel; declined
    ones resume and finish in the browser. Nothing stays paused.
35. Repeat G, L and M.
    Expect: the same results as in Chromium.
