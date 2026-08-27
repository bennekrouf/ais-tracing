# ais-tracing

Follow one value — a correlation id, an order reference — through every
container of an Azure Cosmos DB account, and see where it got to.

It samples your containers, works out which field links documents together,
then draws each step on a timeline. Empty rows matter as much as full ones:
they show where the data hasn't arrived.

---

## Install

Downloads are on the [latest release](https://github.com/bennekrouf/ais-tracing/releases/latest).

### macOS — Apple Silicon

Download [`ais-tracing-macos-arm64.dmg`](https://mayorana.ch/downloads/ais-tracing/latest/ais-tracing-macos-arm64.dmg),
open it, drag **AIS Tracing** to Applications.

Signed and notarized by Apple, so it opens with a normal double-click — no
right-click-to-open, no security warning.

### Windows 10/11 — 64-bit

Download [`ais-tracing-setup.exe`](https://mayorana.ch/downloads/ais-tracing/latest/ais-tracing-setup.exe)
and run it. The installer adds a Start menu entry, installs the Azure CLI if
you don't have it, and offers to launch the app when it finishes.

Windows may warn that the publisher is unknown — the installer isn't
code-signed yet. Choose **More info → Run anyway**.

### Linux — x86_64

```bash
curl -L https://mayorana.ch/downloads/ais-tracing/latest/ais-tracing-linux-x86_64.tar.gz | tar xz
cd ais-tracing-linux-x86_64
sudo ./setup-linux.sh
./ais-tracing
```

`setup-linux.sh` installs WebKitGTK and the Azure CLI (skipping whatever you
already have) and adds a desktop launcher. Debian/Ubuntu, Fedora and Arch are
handled.

---

## First run

ais-tracing has no login of its own — it uses your Azure CLI session:

```bash
az login
```

Then start the app and pick a Cosmos DB account. It samples the containers,
proposes a correlation key, and you paste a value to follow.

If a scan fails with **403**, your account can see the database but not read
its data — Cosmos data access is a separate grant from normal Azure RBAC. The
app offers a button to give yourself the Built-in Data Reader role.

---

## Build from source

Needs [Rust](https://rustup.rs). On Linux, install the build dependencies
first:

```bash
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libxdo-dev \
  libayatana-appindicator3-dev librsvg2-dev pkg-config
```

Then:

```bash
cargo run --release
```

---

## Licence

Source-available under the [PolyForm Noncommercial License 1.0.0](LICENSE).

- **Free** for personal use, learning, research and hobby projects, and for
  charities, schools, universities and government institutions.
- **Commercial use requires a licence** — including a solo consultant using it
  on client work, and an employee using it at their job.
  [Get in touch](https://mayorana.ch/en/contact).

This is deliberately not an OSI-approved open source licence: the source is
public and readable, but companies using it for work buy a licence.

The name, logo and icons are trademarks and are not covered by that licence —
fork it and rebrand it. See [TRADEMARK.md](TRADEMARK.md).
