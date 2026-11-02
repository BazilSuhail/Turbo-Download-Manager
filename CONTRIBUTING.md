<p align="center">
    <img src="assets/tdm-logo.png" alt="Turbo Download Manager logo" width="140px">
</p>

<h1 align="center">Contributing to Turbo Download Manager</h1>

<p align="center">
  <img alt="PRs welcome" src="https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square">
  <img alt="Licence: MIT OR Apache-2.0" src="https://img.shields.io/badge/licence-MIT%20OR%20Apache--2.0-orange?style=flat-square">
</p>

Thank you for helping improve TDM. This guide explains how to report problems, propose changes, set up a development environment, and open a pull request that can be reviewed and merged quickly.

> [!IMPORTANT]
> Please read the [Code of conduct](#code-of-conduct) and the [Pull request checklist](#pull-request-checklist) before you open a pull request. Pull requests that skip the checklist may be closed without review.

## Contents

1. [Code of conduct](#code-of-conduct)
2. [Ways to contribute](#ways-to-contribute)
3. [Reporting bugs](#reporting-bugs)
4. [Security issues](#security-issues)
5. [Proposing a feature](#proposing-a-feature)
6. [Development setup](#development-setup)
7. [Project layout](#project-layout)
8. [Making a change](#making-a-change)
9. [Coding standards](#coding-standards)
10. [Tests](#tests)
11. [Commit messages](#commit-messages)
12. [Pull request checklist](#pull-request-checklist)
13. [Review process](#review-process)
14. [Releases](#releases)
15. [Licence of contributions](#licence-of-contributions)

## Code of conduct

Be respectful and constructive. Assume good intent, criticise code rather than people, and keep discussions focused on the project. Harassment, personal attacks, and discrimination are not allowed, and maintainers may remove comments or contributors who break these rules.

## Ways to contribute

- **Report a bug** that you can reproduce.
- **Improve the documentation**, including this guide, the README, and the feature guide in `docs/`.
- **Fix an issue** labelled `good first issue` or `help wanted`.
- **Review pull requests** by testing them and commenting on what you find.
- **Improve the browser extension** in `extension/`.

For larger changes, open an issue first so the approach can be agreed before you spend time on code.

## Reporting bugs

Open an issue and include:

1. What you did, step by step.
2. What you expected to happen.
3. What actually happened, including any error text or screenshot.
4. Your version (the **About** entry in the **Help** menu), your Windows or Linux version, and your browser if the problem involves capture.
5. The relevant lines from the log or the terminal, if you ran TDM from a terminal.

Search the existing issues first, in case the problem is already reported.

## Security issues

Do **not** open a public issue for a security problem. Report it privately through GitHub's private vulnerability reporting on the repository (the **Security** tab, then **Report a vulnerability**). Describe the problem, how to reproduce it, and the affected version. Maintainers will acknowledge the report and agree on a fix and a disclosure date.

## Proposing a feature

Open an issue that describes:

- The problem the feature solves, from a user's point of view.
- How you imagine it working.
- Any alternatives you considered.

Features that add a new network protocol, change how files are stored, or change the browser extension's permissions need extra discussion before any code is written.

## Development setup

You need:

| Tool       | Version                                                           | Used for                                   |
| ---------- | ----------------------------------------------------------------- | ------------------------------------------ |
| Rust       | stable (edition 2024, Rust 1.85 or newer)                         | the application                            |
| Node.js    | 20 or newer                                                       | building and testing the browser extension |
| Git        | any recent version                                                | source control                             |
| Linux only | the GUI libraries in the [README](README.md#building-from-source) | the windowing system                       |

Clone the repository and build it:

```sh
git clone https://github.com/BazilSuhail/Turbo-Download-Manager.git
cd Turbo-Download-Manager
cargo build
```

`cargo build` also builds the browser extension into `extension/dist/`. To rebuild only the extension, run:

```sh
node extension/build.mjs
```

To run the application from source:

```sh
cargo run
```

## Project layout

| Path                                                  | Contents                                                             |
| ----------------------------------------------------- | -------------------------------------------------------------------- |
| `src/engine.rs`                                       | segmented downloads, resume state, retries                           |
| `src/ftp.rs`, `src/media.rs`                          | FTP downloads, HLS and DASH manifests                                |
| `src/main.rs`                                         | the window, the download list, dialogs, and keyboard handling        |
| `src/ui/`                                             | menus, themes, preferences, icons, and text helpers                  |
| `src/filetype.rs`                                     | file type detection from the first bytes of a file                   |
| `src/store.rs`, `src/model.rs`                        | saved settings and the download list                                 |
| `src/ipc.rs`, `src/native_host.rs`, `src/register.rs` | browser capture and browser registration                             |
| `extension/`                                          | the browser extension (Chrome, Edge, Brave, Vivaldi, Opera, Firefox) |
| `packaging/`                                          | installer templates and packaging metadata                           |
| `tests/`                                              | integration tests that run against local servers                     |
| `docs/`                                               | the feature guide, this guide, and the release guide                 |

## Making a change

1. **Fork** the repository and clone your fork.
2. **Create a branch** from `main`. Use a short name that says what the branch does, for example `fix/retry-after-timeout` or `feat/extension-logo`.
3. **Make the change.** Keep it focused on one problem. Separate refactoring from behaviour changes.
4. **Test it** using the steps in [Tests](#tests).
5. **Commit** using the conventions in [Commit messages](#commit-messages).
6. **Push** your branch and open a pull request against `main`.

## Coding standards

- Write code that matches the style around it. Run `cargo fmt` only on code you changed.
- Keep `cargo clippy --all-targets` free of warnings.
- Do not introduce `unwrap()` or `expect()` on data that comes from the network, a file, or the user. Return an error instead.
- Keep user-visible text short and plain. Use the existing message helpers.
- Do not add a dependency unless it is clearly needed. Explain why in the pull request.
- Do not commit secrets, credentials, personal data, or generated build output such as `target/` or `extension/dist/`.
- For the browser extension, do not request new permissions without an explanation in the pull request.

## Tests

Run these before you open a pull request:

```sh
cargo test
cargo clippy --all-targets
node --test extension/filter.test.mjs extension/media-core.test.mjs extension/tools/tools.test.mjs
```

Add a test for every bug you fix, and for every new rule you add. Tests that need a network connection should use the local test servers in `tests/` rather than real websites.

## Commit messages

Use the [Conventional Commits](https://www.conventionalcommits.org/) style:

```
<type>: <short summary in the imperative mood>

<optional body explaining why, wrapped at about 72 characters>
```

Common types:

| Type       | Use it for                                |
| ---------- | ----------------------------------------- |
| `feat`     | a new feature                             |
| `fix`      | a bug fix                                 |
| `docs`     | documentation only                        |
| `ci`       | GitHub Actions workflows                  |
| `refactor` | code changes that do not change behaviour |
| `test`     | tests only                                |
| `chore`    | maintenance, such as version bumps        |

Examples:

```
fix: keep the download when the server drops the connection
docs: add contributing guide
ci: publish successful platform builds
```

Keep each commit about one change, so that it can be reverted on its own.

## Pull request checklist

Before you request a review, check that:

- [ ] The branch is up to date with `main`.
- [ ] `cargo test` passes.
- [ ] `cargo clippy --all-targets` reports no new warnings.
- [ ] The extension tests pass, if you changed `extension/`.
- [ ] New behaviour has a test, and bug fixes have a regression test.
- [ ] The documentation is updated if behaviour changed (`README.md` or `docs/`).
- [ ] The pull request description explains what changed and why.
- [ ] You tested the change on the platform it affects, and say which platform in the description.
- [ ] Screenshots or a short recording are included for any visible UI change.
- [ ] No generated files, secrets, or personal data are included.

Use this description template:

```
## What changed
<one or two sentences>

## Why
<the problem this solves, with a link to the issue if there is one>

## How it was tested
<the commands you ran and the platform>

## Notes for reviewers
<anything that needs attention, or "none">
```

## Review process

- A maintainer will review your pull request, usually within a few days. Larger changes may take longer.
- Reviewers may ask for changes. Push new commits to the same branch rather than opening a new pull request.
- Please respond to review comments, or say if you disagree and why.
- Maintainers may squash commits when merging.
- A pull request that has had no activity for a month may be closed. You can reopen it at any time.

## Releases

Only maintainers publish releases. The release steps are in [RELEASING.md](docs/RELEASING.md). Do not change version numbers in a pull request unless a maintainer asks you to.

## Licence of contributions

TDM is licensed under the MIT and Apache-2.0 licences (see `LICENSE-MIT` and `LICENSE-APACHE`). By submitting a contribution, you agree that it is licensed under the same terms, and that you have the right to submit it.
