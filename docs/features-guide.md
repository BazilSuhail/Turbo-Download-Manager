# TDM feature guide

This guide explains the features added recently: how to use them, where they live, and what to expect. For installation and build steps, see the main [README](../README.md).

## Contents

1. [Browser extension](#1-browser-extension)
2. [Download buttons on pages](#2-download-buttons-on-pages)
3. [Adding downloads](#3-adding-downloads)
4. [Retries](#4-retries)
5. [Selecting and deleting downloads](#5-selecting-and-deleting-downloads)
6. [Renaming a download before it saves](#6-renaming-a-download-before-it-saves)
7. [File type check](#7-file-type-check)
8. [Keyboard shortcuts](#8-keyboard-shortcuts)
9. [Window, folder, and notification settings](#9-window-folder-and-notification-settings)
10. [Streaming sites (planned)](#10-streaming-sites-planned)
11. [Troubleshooting](#11-troubleshooting)

---

## 1. Browser extension

The extension sends downloads and page media to TDM. It works in Chrome, Edge, Brave, Vivaldi, Opera, and Firefox.

### Install it once

1. Make sure the extension folders exist. `cargo build` creates `extension/dist/chrome` and `extension/dist/firefox`. If they are missing, run `node extension/build.mjs` (Node 20 or newer).
2. Start TDM. In the sidebar, under **BROWSERS**, click **Add**.
3. Choose your browser and click **Add to <browser>**. TDM copies the extension folder path and opens the extensions page.
4. On the extensions page, turn on **Developer mode**, click **Load unpacked**, press Ctrl+V, and click **Select Folder**.
5. The browser's status in TDM changes to **Connected**.

Each browser row in the guide shows its extension folder. **Copy path** copies the full path. **Open folder** opens it in Explorer.

### Update it later

After you change anything in `extension/`, rebuild the packages and reload the extension on the extensions page (the reload icon on the TDM card).

---

## 2. Download buttons on pages

These buttons appear on web pages once the extension is loaded.

### Right-click menu

| Right-click on | Menu item | What it does |
|---|---|---|
| A link | **Download with TDM** | Sends the link to TDM. |
| An image, video, or audio | **Download media with TDM** | Sends the file's address to TDM. |
| Selected text | **Download selected links with TDM** | Sends every link in the selection, one after another. |

### Hover button

- **Video and audio:** move the pointer over a player. A **Download with TDM** button with the TDM logo appears at the top-right of the player.
- **Large images:** move the pointer over an image at least 160×120 px. The same button appears.

Click the button to send the file to TDM. It turns green with **Sent to TDM** when TDM accepts it, or red if TDM isn't running or the file can't be sent. Click the **×** beside it to hide the button for that item.

Keyboard users: focus a video or audio player, press Tab to move into the button, and press Escape to return to the player.

### Limits

- The buttons only work with files that have a direct web address. Streams that TDM can't read directly won't show a working button.
- Hover buttons don't appear on fullscreen players.
- TDM does not download YouTube videos or let you choose a stream quality from streaming sites.

---

## 3. Adding downloads

### Paste several links

Copy several links at once (for example, from an email), then paste them into the link box, or press Ctrl+Shift+V. When two or more links are on the clipboard, TDM opens the batch dialog with them filled in, one per line.

### Batch and mirrors

- Use `[001-120]` or `[a-z]` in a link to add a range.
- Add **mirror links** in the batch dialog, one per line. If a connection keeps failing, TDM tries the mirrors.

### Capture prompt

When the browser hands a download to TDM, a prompt asks what to do: **Download**, **Add to queue**, or **Cancel**.

For **images and videos**, the prompt also offers:
- An editable **file name**. The extension stays fixed.
- A **Location** line showing where the file will be saved, with a **Change** button to pick another folder.
- A preview of the cleaned name, shown under the field.

If you don't change the name, the file saves to the default folder under its default name. TDM never overwrites an existing file. If the name is taken, the new file is saved as `name (2).ext`.

---

## 4. Retries

When a download fails because of a network problem (a timeout, a dropped connection, or a server error such as 503), TDM retries it automatically:

- Each retry waits longer than the last: 5 seconds, then 10, then 20, up to 5 minutes.
- The download shows **Retrying in N s** while it waits.
- A download that fails for a permanent reason (such as a 404 Not Found) is not retried.

**Retry failed downloads** (Ctrl+R, or the Downloads menu) restarts every download that has failed.

Pressing **Pause everything** or **Stop queue** cancels any pending retries.

---

## 5. Selecting and deleting downloads

### Select mode

- Press **Ctrl+A** or use **Select** from the toolbar. Every visible row gets a checkbox.
- Click rows to tick them, or press Space on the highlighted row.
- Press **Done** or Escape to leave select mode.

### Ctrl+click

- Hold **Ctrl** and click a row to add or remove it from the selection. This works without entering select mode.
- A bar appears above the list while rows are selected. It shows the count, an **Also delete files from disk** tick, **Delete N**, and **Clear**.

### Deleting

- Delete removes the download from the list. Its file stays on disk unless you tick **Also delete files from disk**.
- The confirmation depends on how many downloads you are deleting:
  - **One download:** a simple confirmation.
  - **2–9 downloads:** a confirmation with the count.
  - **10 or more, or any download that is still running:** you must type the number of downloads to confirm.
- Downloads hidden by the search box are never deleted by accident. Changing the search or the filter clears the selection.
- A download whose file is being renamed can't be deleted until the rename finishes.

Pressing Delete with rows selected deletes the selection only when the highlighted row is part of it. Otherwise it deletes only the highlighted row.

---

## 6. Renaming a download before it saves

For image and video downloads, the capture prompt lets you rename the file. The extension is fixed, so only the name before it changes.

Renaming a finished download later:
- Open the download's warning dialog (see [File type check](#7-file-type-check)).
- Click **Rename to .ext** if the offered name is correct.

Renames never overwrite an existing file.

---

## 7. File type check

After a download finishes, TDM reads the start of the file and compares its real type with its name.

What you may see:

- **A badge** on the row, for example **PNG** or **HTML page**. A muted badge means the type matches.
- **A warning** when the name doesn't match the content. For example, a ZIP file named `setup.exe` is flagged.
- **A web-page warning** when the server sent an HTML page instead of the file you asked for, for example a login page saved as `photo.png`. The dialog offers **Open the page in browser**, which opens the original address.
- **A rename offer** for clear mismatches only. TDM never renames executable files, and never renames files on its own.

Plain text, CSV, JSON, and similar files are never flagged.

---

## 8. Keyboard shortcuts

| Key | Action |
|---|---|
| Ctrl+N | New download |
| Ctrl+Shift+V | Paste link from clipboard |
| Ctrl+F | Search downloads |
| Ctrl+A | Select mode (select all visible rows) |
| Ctrl+P | Pause everything if anything is running, otherwise start the queue |
| Ctrl+R | Retry failed downloads |
| Up / Down | Move the highlight |
| Home / End | Go to the first or last row |
| Enter | Open a finished file, or pause/resume the highlighted one |
| Space | Pause or resume the highlighted download (or tick it in select mode) |
| Delete | Delete the highlighted download, or the selection (see [Deleting](#deleting)) |
| F2 | Show the file in its folder |
| Escape | Close the open dialog, leave select mode, or clear the selection |

Shortcuts don't fire while you type in a text box.

---

## 9. Window, folder, and notification settings

- **Window size and position:** TDM remembers its window size and position. If the saved position is off-screen (for example, after unplugging a monitor), the window opens on the main screen.
- **Default folder:** the folder chip in the top bar shows where new downloads go. Click it, or use **Folder → Change default download folder**, to choose another. **Open download folder** opens it in Explorer.
- **Taskbar flash:** when a download finishes while TDM is in the background, its taskbar button flashes. Turn this off under **Preferences → Downloads → Flash the taskbar when a download finishes**.
- **After the queue:** under **Preferences → Connections & limits**, choose whether TDM stays open, closes, or shuts the PC down 60 seconds after the queue finishes.

---

## 10. Streaming sites (planned)

> **Status: not implemented yet.** This section is a placeholder for a later release.

What works today:
- Direct video and audio files with a web address, through the hover button and the right-click menu (section 2).
- HLS and DASH manifests that TDM can read directly.

What is planned for later:
- A streaming section in the sidebar that lists media the extension has found on the current page, so you can review it before sending.
- Choosing a quality from the streams a page offers, for sites where the streams are open and can be downloaded legally.

What is not planned:
- Downloading YouTube videos, or any stream protected by DRM or access controls.
- Extracting protected streams to offer quality options on streaming services.

Once this section is implemented, update its status line and move the details into the sections above.

## 11. Troubleshooting

| Problem | What to do |
|---|---|
| The extension shows **Not added yet** | Load it again from the extensions page (section 1). Check the folder exists. |
| Build fails with "Access is denied" on `tdm.exe` | Close the running TDM window, then build again. |
| No **Download with TDM** button on a video | The player must be at least 120×60 px and show a direct web address. Reload the page. |
| The hover button appears on images but not on a video | Only players with a direct address get the button. Use right-click instead. |
| A file has the wrong extension after download | Look at its warning badge and use **Rename** if the offered name is correct. |
| Windows warns when running an installer | The installer isn't code-signed yet. Choose **More info**, then **Run anyway**. |
| A download saved as a web page | The server sent an HTML page. Use **Open the page in browser** from the warning dialog and download from there. |
