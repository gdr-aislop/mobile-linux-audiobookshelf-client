# Mockup sources

These `.dc.html` files (plus `canvas.json`, the layout manifest) are the source artboards behind
the published design canvas: https://claude.ai/artifact/TLd4NwbXw7EpyM8HtvYKkh

Each file is one screen from `docs/design/ui-spec.md`:

| File | Screen |
|---|---|
| `Welcome.dc.html` | Welcome / Server login — first-launch state |
| `WelcomeError.dc.html` | Welcome / Server login — failed-connection state |
| `Main.dc.html` | Home |
| `Library.dc.html` | Library browse (incl. the view-options sheet) |
| `ItemDetail.dc.html` | Item detail (incl. the download-scope sheet) |
| `Player.dc.html` | Full player |
| `Settings.dc.html` | Settings (incl. Servers group) |
| `Connection.dc.html` | Per-server Connection settings |

## Updating

Edit the relevant `.dc.html` (or add a new one + update `canvas.json`), then re-seed and republish
using the `design` skill's helper against the payload template it ships with — see that skill for
the exact commands. Republishing to the same artifact URL keeps this one canvas current rather
than creating a new link.

The large, fully-seeded `lissen-linux-mockups.html` (the actual file handed to the `Artifact` tool
to publish) is a generated build artifact — regenerated from these sources on every update — and
isn't committed here.
