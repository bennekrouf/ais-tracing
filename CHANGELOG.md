# Changelog

What changed in each release of **AIS Tracing**, the desktop app that follows
one correlation id through every container of an Azure Cosmos DB account.

The public version of this page — with the download for each release — lives at
<https://mayorana.ch/en/apps/ais-tracing/releases>. It is generated from this
file by `scripts/changelog_to_json.py`, so this file is the only place a
release note is written.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each heading is dated on the day its tag was pushed. Releases that carried only
build or packaging work say so rather than being hidden: the version numbers a
user sees in the update prompt should all be accounted for.

## [0.1.22] - 2026-09-05

### Changed

- Error handling and feedback across the scan, trace and account screens: a
  failure says what failed and what to do about it, instead of leaving a view
  empty.
- Long lane names are truncated cleanly rather than pushing the timeline out
  of shape.

### Fixed

- The Windows build script handles cross-compilation correctly, and CI now
  blocks on clippy warnings.

## [0.1.21] - 2026-09-03

### Changed

- CI pipeline only — no user-visible change.

## [0.1.20] - 2026-09-03

### Fixed

- "AIS Tracing" is capitalised the same way in the window title and the
  in-app headers.

## [0.1.19] - 2026-09-02

### Changed

- The update check sends a versioned User-Agent, which is what makes
  per-version adoption visible in the download logs — the number that says how
  many people are still on a build with a bug that is already fixed.

## [0.1.18] - 2026-08-31

### Changed

- Internal: Azure sign-in consolidated into one `start_login` path shared by
  every screen that can trigger it.

## [0.1.17] - 2026-08-31

### Added

- Windows support: a signed installer, the Azure CLI installed for you if it
  is missing, and a Start menu entry.

## [0.1.16] - 2026-08-29

### Added

- The update check resolves the artifact for the platform it is running on, so
  the banner offers the macOS, Windows or Linux build directly instead of the
  download page.

## [0.1.15] - 2026-08-29

### Fixed

- Long lane names and field names wrap instead of overflowing their column.

## [0.1.14] - 2026-08-28

### Added

- Scanning a Cosmos DB account reports why it failed: an expired Azure session
  is named as one, rather than surfacing as an account with no containers.

## [0.1.13] - 2026-08-27

### Changed

- Release safety: the pipeline fails outright when the deploy secrets are
  missing, rather than reporting success for a release nobody can download.

## [0.1.12] - 2026-08-27

### Changed

- Builds are distributed from mayorana.ch instead of GitHub Releases. Every
  release publishes a `latest.json` carrying the `sha256` of each artifact, so
  a download can be verified against a source that does not deliver it.
- Licensed under PolyForm Noncommercial 1.0.0: free for personal, educational
  and non-profit use; commercial use requires a licence.

## [0.1.11] - 2026-08-27

### Fixed

- The macOS binary is signed before packaging and checksummed after, so the
  published `sha256` matches the file you actually download.

## [0.1.10] - 2026-08-27

### Fixed

- The app adopts the login shell's `PATH`, so `az` is found when it is launched
  from Finder rather than from a terminal.

## [0.1.9] - 2026-08-27

### Fixed

- A release started from the Actions UI tags the version it prepared, instead
  of the branch it was dispatched from.

## [0.1.8] - 2026-08-26

### Added

- Multi-window support: several traces open side by side, each in its own
  window.

## [0.1.7] - 2026-08-26

### Fixed

- The release script no longer hangs waiting for a confirmation prompt when it
  runs without a terminal.

## [0.1.6] - 2026-08-25

### Added

- A lightweight update check at startup, with a banner when a newer build is
  available. It reads the manifest published beside the builds, so it keeps
  working regardless of where the source lives.

---

Releases before 0.1.6 predate this file. Their tags remain on
[GitHub](https://github.com/bennekrouf/ais-tracing/releases).
