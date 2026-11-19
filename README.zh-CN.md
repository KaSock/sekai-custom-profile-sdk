# Sekai Custom Profile SDK

[English](README.md) | 简体中文

这是一个非官方的 Project SEKAI 自定义名片编辑与渲染工具包。它既能用 CPU 原生渲染，也能用浏览器 WebGL2 渲染。文字和排版尽量贴近游戏；未覆盖的行为和游戏更新可能造成差异。

## 能做什么

- 编辑游戏格式的名片数据，支持撤销、重做和导出。
- 把名片渲染成图片，也能导出动态名片。
- 用浏览器编辑名片并即时预览。
- 由接入方提供字体、图片和游戏数据。

BOT 服务可以组合使用浏览器渲染静态页、原生程序导出动态名片。

## 包

- [`sekai-profile-core`](crates/sekai-profile-core)：共用的名片数据、排版和编辑逻辑。
- [`sekai-profile`](crates/sekai-profile)：原生渲染和素材处理。
- [`sekai-profile-cli`](crates/sekai-profile-cli)：提供 `render`、`prepare`、`serve` 命令。
- [`@empty-sekai/sekai-custom-profile-sdk`](crates/sekai-profile-wasm/README.zh-CN.md)：浏览器渲染和编辑接口。

可以查看[浏览器演示](crates/sekai-profile-wasm/demo/README.md)，或安装浏览器包：

```sh
npm install @empty-sekai/sekai-custom-profile-sdk
```

构建 Rust 程序需要 FreeType。浏览器运行时通过 Docker 执行 `crates/sekai-profile-wasm/build.sh` 构建。

## License

仓库采用 AGPL-3.0-only（[LICENSE](LICENSE)）。npm 包另附一项有限的附加许可，允许把未修改的包
与在浏览器中运行的前端代码链接（[LICENSE-EXCEPTION](crates/sekai-profile-wasm/LICENSE-EXCEPTION)）。
修改后的构建、服务端使用以及其他任何非浏览器使用仍受完整 AGPL 约束，包括网络交互场景下的源代码
提供义务。
