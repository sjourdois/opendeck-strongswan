# opendeck-strongswan

Native **Rust** [strongSwan](https://strongswan.org/) VPN plugin for
[OpenDeck](https://github.com/nekename/OpenDeck): a key per `swanctl` connection,
pressed to connect it and pressed again to disconnect it.

It is a plain Rust binary launched by OpenDeck, speaking the OpenAction / Elgato
Stream Deck WebSocket protocol through the
[`openaction`](https://crates.io/crates/openaction) crate, and driving charon
over its VICI socket through [`rsvici`](https://crates.io/crates/rsvici).

> This repository is the **source**. The installable plugin id is
> `fr.jourdois.strongswan.sdPlugin`, and the `.sdPlugin` bundle is a build
> artifact assembled from `assets/` + the compiled binaries.

## Features

| Action | Controllers | What it does |
|--------|-------------|--------------|
| **VPN Connection** | Keypad · Encoder | Connect or disconnect one strongSwan connection |

The key shows one of four states, each an ordinary OpenDeck state whose image
and title you can change in OpenDeck's editor — the plugin only switches between
them, it never draws over them:

| State | When |
|-------|------|
| **Disconnected** | the connection has no SA |
| **Connected** | its IKE_SA is established with a CHILD_SA installed |
| **Connecting** | an SA is being negotiated or deleted, from this plugin or from anywhere else (`swanctl`, a status-bar menu, DPD) |
| **Unavailable** | charon isn't reachable, or the connection isn't loaded |

A press on a disconnected connection brings up every CHILD_SA it defines; a
press on a connected one — or one still connecting, which cancels it — tears its
IKE_SA down. When a connect or disconnect fails, the key shows OpenDeck's alert
and the plugin log carries charon's error message; its step-by-step messages
are logged at debug level only. Picking a
connection in the property inspector titles the key after it; rename it in
OpenDeck afterwards and the title stays yours.

### Exclusive connections

Two connections can be marked **exclusive**: connecting one first disconnects
the other. That is what two full-tunnel VPNs need, since both claim the default
route. The relation is symmetric and shared by every key — it lives in the
plugin's global settings, not in one key — so ticking `work2` under
*Exclusive with* on the `work1` key shows `work1` ticked on the `work2` key.

It applies to connects made from this plugin; one made with `swanctl` directly
does not disconnect anything.

The keys follow charon live: `ike-updown` / `child-updown` events redraw them at
once, and a poll catches what raises no event (negotiations in progress, a
`swanctl --load-all` that changed the connection list). A restarted charon is
picked up again on its own.

> **Linux only**, and **swanctl / VICI only**: the legacy `ipsec` stroke
> interface and NetworkManager-strongswan are not supported.

## Requirements

- A running **charon** (`charon-systemd`, from the `strongswan` / `charon-systemd`
  packages) with the **vici** plugin, which Debian loads by default.
- Access to its socket, `/run/charon.vici`. charon recreates it owned by
  `root:root` on every start and the vici plugin has no option to change that,
  so a drop-in on the service is the reliable way to open it to a group the user
  is in, e.g. `netdev`:

  ```ini
  # /etc/systemd/system/strongswan.service.d/vici-group.conf
  [Service]
  ExecStartPost=/bin/chgrp netdev /run/charon.vici
  ExecStartPost=/bin/chmod 0660 /run/charon.vici
  ```

  Anyone who can open this socket controls charon entirely — loading
  connections and keys included — so only hand it to a group you'd trust with
  that.

`opendeck-strongswan --status` prints what the plugin sees — each loaded
connection and its state — which is the quickest way to check the socket is
readable.

## Install

**Manual install** (until published to the OpenDeck plugin store):

1. Build the binary (see below).
2. Assemble the plugin directory `fr.jourdois.strongswan.sdPlugin/`:
   ```
   fr.jourdois.strongswan.sdPlugin/
   ├── manifest.json            (from assets/)
   ├── icons/                   (from assets/)
   ├── propertyInspector/       (from assets/)
   └── <target-triple>/bin/opendeck-strongswan
   ```
   Everything but the binary is the contents of `assets/`, copied as-is.
3. Copy that directory into `~/.config/opendeck/plugins/` and restart OpenDeck.

`deno run -A build.ts ~/.config/opendeck/plugins/fr.jourdois.strongswan.sdPlugin x86_64-unknown-linux-gnu`
does steps 1–3 but the restart in one go.

## Build from source

```bash
cargo build --release      # -> target/release/opendeck-strongswan
```

No system library is needed: VICI is spoken natively.

CI builds the release `.sdPlugin` bundle (x86_64 + aarch64 Linux) — see
`.github/workflows/release.yml`. Pushing a `v*` tag attaches the zipped bundle
to the GitHub release; `.github/workflows/ci.yml` runs fmt + clippy + build on
every push.

## License

GPL-3.0-or-later — see [`LICENSE`](LICENSE).

## Credits

- Plugin SDK: [`openaction`](https://crates.io/crates/openaction) / the
  [OpenAction API](https://openaction.amankhanna.me/) and
  [OpenDeck](https://github.com/nekename/OpenDeck) by Aman Khanna (nekename).
- VICI client: [`rsvici`](https://github.com/chitoku-k/rsvici) by Chitoku.
