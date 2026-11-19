# Sekai Custom Profile SDK

English | [简体中文](README.zh-CN.md)

An unofficial toolkit for editing and rendering Project SEKAI custom profile cards. It offers a native CPU renderer and a WebGL2 browser renderer. Rich text and layout aim to match the game, but unsupported behavior and future game updates may differ.

## What it does

- Edit game-compatible profile data, then undo, redo and export changes.
- Render profile pages as images or export animated cards.
- Use the browser renderer for interactive editing and previews.
- Load fonts, images and masterdata through providers supplied by your app.

The BOT service can combine GPU-rendered browser pages with native animation export.

## Packages

- [`sekai-profile-core`](crates/sekai-profile-core): shared profile data, layout and editing logic.
- [`sekai-profile`](crates/sekai-profile): native renderer and asset handling.
- [`sekai-profile-cli`](crates/sekai-profile-cli): `render`, `prepare` and `serve` commands.
- [`@empty-sekai/sekai-custom-profile-sdk`](crates/sekai-profile-wasm/README.md): browser renderer and editing API.

See the [browser demo](crates/sekai-profile-wasm/demo/README.md) or install the browser package with:

```sh
npm install @empty-sekai/sekai-custom-profile-sdk
```

Rust builds require FreeType. The browser runtime is built with `crates/sekai-profile-wasm/build.sh` in Docker.

## License

The repository is licensed under AGPL-3.0-only ([LICENSE](LICENSE)). The npm
package also carries a limited additional permission for linking the
unmodified package into front-end code that runs in a web browser
([LICENSE-EXCEPTION](crates/sekai-profile-wasm/LICENSE-EXCEPTION)). Modified
builds, server use and any other non-browser use remain under the full AGPL,
including its network-use source requirement.
