# Changelog

All notable changes to this project are documented in this file. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0]

### Added

- **VPN Connection** action (Keypad · Encoder): connect or disconnect one
  strongSwan connection over charon's VICI socket. The key is a globe in a
  colour of your choice over the connection's name or a label of yours, and
  shows the connection's state — disconnected, connected, connecting,
  unavailable — kept live from charon's events.
- **Exclusive connections**: pairs of connections, kept in the plugin's global
  settings, where connecting one first disconnects the other.
- `--status` prints the connections and their state as the plugin sees them.
