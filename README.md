# hack-shell

A GPU terminal for EndeavourOS / Plasma Wayland. Cells are drawn as instanced quads on the GPU (Vulkan, Intel iGPU preferred). Selecting text publishes it on the Wayland **primary selection**, so another window can paste it with a middle click. Copy-on-select also fills the normal clipboard.

## Build

```sh
./install.sh
```

Or, without installing:

```sh
. "$HOME/.cargo/env"
cargo run --release
```

## Paste

- Drag-select text. Middle-click in Kate, Firefox, or another terminal pastes it.
- Middle-click here pastes the primary selection from whichever window owns it.
- Ctrl+Shift+C / Ctrl+Shift+V use the regular clipboard.
- Copy-on-select is on by default (Profiles).

## Keys

| Shortcut | Action |
| --- | --- |
| Ctrl+Shift+T / W / N | New tab, close tab, new window |
| Ctrl+Shift+D / E | Split right / down |
| Ctrl+Shift+arrows | Focus another pane |
| Ctrl+PageUp / PageDown | Switch tab |
| Ctrl+Shift+F | Find |
| Ctrl+Shift+K | Clear scrollback |
| Ctrl+Plus / Minus / 0 | Zoom |
| F11 | Fullscreen |
| Ctrl+click | Open URL |

Config is written to `~/.config/hack-shell/config.json`.

To make this Plasma's default terminal:

```sh
kwriteconfig6 --file kdeglobals --group General --key TerminalApplication hack-shell
kwriteconfig6 --file kdeglobals --group General --key TerminalService hack-shell.desktop
```
