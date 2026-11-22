# rodeo (Windows fork)

> Round up your terminals.

Fork of [lukeed/rodeo](https://github.com/lukeed/rodeo) with native Windows support. The `windows` branch is the default and carries every release; `main` tracks upstream for rebase pulls.

rodeo is a keyboard-first terminal UI for [herdr-core](https://github.com/herdrdev/herdr-core) (`hc`). It shares spaces, tabs & panes with the Herdr desktop app. Make a split in rodeo and it shows up in the app, and the other way around.

`hc` owns every program. rodeo only owns the UI, so you can close it, open it again, or run two at once — your terminals keep running the whole time.

## Features

* Workspaces, tabs & splits, all from the keyboard (`ctrl+b` prefix) or the mouse
* Shares its layout with the Herdr desktop app & other rodeo clients, each at its own resolution
* Agent states (blocked, working, done, idle) at a glance, with toasts & sounds (both opt-in)
* Other machines through `hc`'s machine links
* Worktrees, popups, copy mode, image paste, links & kitty graphics

## Install

1. Install [`hc`](https://github.com/herdrdev/herdr-core) 0.1.18 or newer, on your `PATH`.
2. Download `rodeo-x86_64-pc-windows-msvc.zip` from [Releases](https://github.com/SanteriSuomi/rodeo/releases/latest) and extract `rodeo.exe` somewhere on your `PATH`.

Or build from source: Rust stable MSVC (1.99+) and [zig 0.15.2](https://ziglang.org/download/) on the build's PATH (build-time only, for `libghostty-vt-sys`), then `./build.ps1`.

## Usage

```sh
:$ rodeo                    # open on the default hc session
:$ rodeo --session work     # open on another hc session
```

Detach with `prefix+q`. Your terminals keep running in `hc`, and come right back when you open rodeo again. Press `prefix+?` to see every key bind.

## Config

rodeo reads `~/.config/rodeo/config.toml` with Herdr TUI's schema. On first start it copies `~/.config/herdr/config.toml` over once, if it exists and its own file does not; it never touches the Herdr file.

```sh
:$ rodeo --default-config   # print the full default config
:$ rodeo config check       # check your config
```

## Coming from Herdr TUI?

rodeo started as Herdr TUI 0.9.5 on `hc`. Same key binds, same screen, same config schema. It drifts over time: no plugins, no self-update, no onboarding; agents report their own state to `hc` (OSC 7501), and sessions, restore & remote machines belong to `hc`. [`docs/parity.md`](docs/parity.md) has the full list, row by row.

## Development

```sh
:$ git clone https://github.com/SanteriSuomi/rodeo.git && cd rodeo
:$ mise install              # pinned Rust & zig
:$ cargo test --workspace    # starts its own isolated hc core
:$ ./build.ps1               # release build + install
```

The tests never touch your own terminals. On Windows the ungated suites run (`cli/e2e/machines/notify/popups/worktrees` are unix-only); CI runs the same suite on every push. Start with [`docs/plan.md`](docs/plan.md), [`docs/architecture.md`](docs/architecture.md) and [`docs/releasing.md`](docs/releasing.md).

## License

MIT © Luke Edwards
