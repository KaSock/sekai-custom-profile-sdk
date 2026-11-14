# 浏览器文字画采样修复

代码标记：`FIX-BROWSER-SDF-01`。

## 原因与修复方式

部分静态名片使用放大、旋转的 `⌒`、`●`、三角等文字拼成人物画。浏览器原先统一生成
75px 字号的 SDF 字形，再放大绘制；复杂轮廓的发尖和弧线会暴露基础采样的精度限制。
原生渲染已有大倍率字形的高分辨率采样替换，浏览器原先没有对应步骤。

本次保留 75px 基础图集完成排版，再根据最终字形四边形计算实际放大倍率。超过 3 倍时，
按字号档位生成更高分辨率的字形，替换绘制采样。距离范围随采样字号同比增大，并使用与
原生相同的字体空间网格对齐方式，保留原有字距、断行、锚点和描边等材质参数。

普通字号继续使用原图集。相同字体、字符在同一场景中只请求所需的最高档位；后续场景可
复用字形缓存。采样字号与距离范围进入缓存键，原 75px 缓存键保持兼容，无需删除原生
`atlases/` 或玩家资料缓存。

## 适用范围与资源限制

- 默认用于运行时生成图集、且没有动态程序的静态页面。BOT 组合模式无需增加配置。
- 动态页面保持现有路径，继续由原生程序负责最终导出。
- 预生成图集路径保持原样，不混用它的页面与运行时图集。
- 高分辨率变体与基础字形共用图集池。通用 SDK 默认 6 页；BOT 浏览器组合模式设为
  16 页，每页 2048×2048 的 R8 图集，像素上限约 64 MiB。该上限不包括栅格化临时内存、
  GPU 纹理或其他缓存。
- 单字形尺寸按图集页限制，采样字号最高 4096。达到图集预算后输出警告，保留已完成的
  高清替换；当前字形最多降一档重试，仍无法放入就回退基础采样并继续处理其他字形。
- 采样字号按基础字号的整数倍升档，避免倍率刚跨过 4 倍时直接生成 8 倍采样。
  每页输出 `requested`、`upgraded`、`downgraded`、`fallback` 和 `atlas_pages` 统计。
- 首次生成高清字形会增加 `prepare_ms`；缓存保留时可复用，图集淘汰或重启后可能重新生成。
  不承诺与原生逐像素一致，也不承诺修复后的耗时固定不变。

SDK 调用方需要与旧采样对照时，可在 `createProfileScene` 的选项中设置：

```ts
sdf: { highResolution: false }
```

这是 SDK 选项，不是 `bot_service.json` 配置字段。

## 涉及代码

| 位置 | 修改内容 |
| --- | --- |
| `crates/sekai-profile-wasm/src/highResolutionSdf.ts` | 倍率判断、采样档位、与原生一致的网格对齐、缓存租约与预算回退。 |
| `crates/sekai-profile-wasm/src/renderer.ts` | 静态场景排版后、GPU 上传前接入高清采样；提供对照开关。 |
| `crates/sekai-profile-wasm/src/fontSdfAtlas.ts` | 支持指定采样字号，共用基础图集池；准备失败时释放取得的租约。 |
| `crates/sekai-profile-wasm/src/glyph_plan.rs` | 采样字号与距离范围配套，区分高清缓存键，保持基础缓存兼容。 |
| `crates/sekai-profile-wasm/src/lib.rs` | 新增指定字号的字形规划和生成接口，FreeType 与 SDF 使用实际采样参数。 |
| `crates/sekai-profile-wasm/src/protocol.ts`、`src/worker.ts` | 将采样字号传入 Worker 和新 WASM 接口；旧接口参数不变。 |
| `crates/sekai-profile-wasm/src/main.rs`、`build.sh` | 保留并导出新增 WASM 接口。 |
| `crates/sekai-profile-wasm/scripts/test/high-resolution-sdf.test.mjs` | 验证倍率、几何对齐、缓存复用、预算回退、取消和错误时的资源释放。 |

## 构建与部署

在源码仓库根目录执行：

```powershell
.\build-browser-bot.ps1 -FromSource
```

构建完成后，停止部署机的渲染服务，完整更新 `runtime/browser-sdk/dist/`，包括所有子目录，
再按原命令重启服务。新增 JS 与 WASM 接口必须配套；仅替换 WASM 或仅编译 TypeScript
不能完成本次更新。

本次画质修复无需修改 Python 服务、BOT 代码或现有组合模式配置，也无需因此重建原生 exe。
部署需同步更新仓库的 `bot_browser.py`，使浏览器统计日志进入 INFO 级别；无需改 BOT 项目
或现有配置，也无需因此重建原生 exe。此前动画优化若尚未部署，仍需单独按原生构建流程更新。

## 验证重点

使用相同玩家资料、输出尺寸和 JPEG 质量，对比浏览器与原生结果。复现资料中的第 9、10 页
重点检查发尖、眼部细线、弧线接缝，以及是否出现文字位置或描边粗细偏移。先测首次请求，
再重复渲染，分别记录 `prepare_ms` 和整批返回耗时。

本次在源码侧进行 TypeScript 检查、Node 测试和 Rust 格式检查；未编译 Rust/WASM，
未构建部署产物。实际 GPU 画面与耗时仍需部署后对照验证。
