# BOT 本地名片渲染部署

本 fork 增加 `render_profile_media` NDJSON 接口和本地 HTTP 包装服务。
按页面 seq 排序，静态页面返回 JPEG，支持的动态页面返回 MP4。
原有 `render`、`prepare`、`serve` 接口仍保留。

## 1. 构建

Windows x64：准备 Rust stable MSVC、Visual Studio C++ Build Tools、CMake、
NASM 和已 bootstrap 的 vcpkg。在此仓库根目录运行：

```powershell
.\build-bot.ps1 -VcpkgRoot 'D:\Tools\vcpkg' -InstallDependencies -Test
```

脚本安装 `x264:x64-windows`、`freetype:x64-windows` 和 `pkgconf:x64-windows`，
设置本机库路径，启用 `animation-export` 并构建 Release。
Windows 使用 vcpkg 的 FreeType，避免上游 bundled 构建缺少 zlib 头文件的问题。
生成 `target/release/sekai-profile.exe`，部署时将旁边的运行时 DLL 一并带走。
构建脚本的操作目录仅为本仓库与指定的 vcpkg；不会修改 BOT 目录。

也可以提交改动后，在自己的 GitHub fork 的 Actions 中手动运行
`Build BOT renderer (Windows)`，下载 `allium-bot-windows-x64` artifact。
这是专门的构建工作流，不会发布上游 npm 包或网站。

Linux 构建：先安装系统的 FreeType/x264/CMake/C 编译工具和 NASM，运行：

```text
cargo build --locked --release -p sekai-profile-cli --features animation-export
```

如 x264 不在默认编译/链接目录，可指定 `X264_INCLUDE_DIR` 和 `X264_LIB_DIR`；
MSVC 的 vcpkg 库名为 `libx264`，构建脚本会设置 `X264_LIB_NAME`。

## 2. 运行目录与字体

渲染器运行目录可以放在 BOT 目录外：

```text
AlliumRenderer/
  sekai-profile.exe
  *.dll
  bot_service.py
  bot_service.json
  fonts/
  atlases/
    cn/
    jp/
    tw/
    en/
```

字体请自行获得使用授权，放入 `fonts`。渲染器按以下文件名查找：

- `FOT-RodinNTLGPro-DB.ttf` 或 `FOT-RodinNTLGPro-DB.otf`
- `FOT-SkipProN-B.otf`
- `FOT-PopHappinessStd-EB.otf`
- `FOT-YurukaStd-UB.otf`
- 回退字体：`SourceHanSansSC-Medium.otf` 或 `SourceHanSansSC-Regular.otf`

国服字体族会映射到上述文件名；不要仅仅把不相同的字体改名后冒充原字体。
如果使用御神签：还需 `FOT-Omikuji.otf`、`FOT-UDMinchoPro-B.otf`、
`Roboto-Regular.ttf`、`NotoSansCJK-Regular.ttc`，详见 `fonts-and-assets.md`。

首次请求某个区服时会执行 `prepare` 构建完整字体和形状图集；日志会明确提示。
这可能比平时渲染更久，超时可增大服务的 `render_timeout_seconds`，
并同步增大 BOT 的 `timeout_seconds`。缺字体时明确报错，不会标记准备完成。
以后复用图集；字体文件或字体/形状表变更会重新准备。

## 3. 服务配置

将 `bot_service.example.json` 复制为 `bot_service.json`，填写：

- `executable`：构建好的 exe；artifact 根目录部署时改为 `sekai-profile.exe`。
- `bot_root`：生产 BOT 的根目录，绝对路径；示例路径只是占位符。
- `bot_url`：BOT FastAPI 实际监听的本机地址和端口，例如 `http://127.0.0.1:8080`。
- `font_dir`、`atlas_dir`：字体和图集目录；相对路径基于配置文件所在目录。
- `token`：随机令牌；必须与 BOT 配置相同，可自行生成，不要提交到 Git。
- `source_url`：你自己的 renderer fork 源码地址，建议指向部署的提交或 tag；
  BOT 将在结果中显示，提供对应版本源码的获取方式。
- `max_concurrent_renders`：跨区服并行数量，默认 1。
- `asset_cache_mb`：每个区服 renderer 的编码图片缓存上限，默认 256 MiB；
  不包含字体图集、解码图片和渲染临时内存。区服进程按需启动。

服务 Python 需要 3.10 或更新版本，依赖 `aiohttp`：

```powershell
python -m pip install aiohttp
python .\bot_service.py --config .\bot_service.json
```

服务只监听 `127.0.0.1:45557`。检查运行状态：

```powershell
Invoke-RestMethod http://127.0.0.1:45557/health
```

运行此服务的 Windows 用户需能只读访问 BOT 的 MasterData 文件，
并能读写自己的图集目录。不要把本地渲染端口映射到公网。

## 4. BOT 配置与资源复用

将 BOT 示例配置中的 `profile_renderer` 段加入生产 `config/sekai/sekai.yaml`：

```yaml
profile_renderer:
  enabled: true
  url: "http://127.0.0.1:45557"
  token: "与服务相同的随机令牌"
  timeout_seconds: 600
  static_url: "https://profile-assets.emptysekai.dev/game-profile/cn/6.0.0/renderer-static"
```

BOT 重新加载修改后的代码后，渲染指令先通过现有个人信息 API 获取
当前区服、当前指定账号的最新完整 profile；失败不会悄悄用旧 profile。
支持 `u1/u2` 与区服前缀，例如 `/cncp u2`、`/jpcp`。

渲染器直接读取 `bot_root/data/sekai/assets/masterdata/{region}`；
BOT 先通过自己的 MasterData 管理器补齐渲染需要的表。服务比较文件
修改时间/大小，有变化才 reload，不会每次重新加载全部 MasterData。
没有单独维护渲染器版的 MasterData 快照。

动态游戏图片通过 BOT 的 `/internal/profile-renderer/...` 适配接口读取：
先尝试已有的 `_rip`/普通缓存路径，缺少时使用 BOT 原有数据源下载。
图片仍落在 BOT 的缓存目录，renderer 仅做内存缓存，没有第二份磁盘图片库。

渲染器专用的静态 UI 图（特殊卡框、遮罩等）不一定全部存在于 BOT。
同名资源优先使用 BOT 的 `static_images`；其余从 `static_url` 下载到
BOT 的 `static_images/profile_renderer` 子目录。它们是必要的新增素材，
不能拿尺寸/用途相近的 BOT 图片替换。上面 URL 来自上游 demo 的默认
静态素材源；可换成自己托管的同结构资源，留空则要求本地文件齐全。
字体/形状图集属于新生成的数据，也会额外占空间。

BOT 资源接口和渲染服务均需同一个 Bearer 令牌；令牌通过子进程环境
变量传入，不会被拼接到授权 URL 或普通素材源 URL。

## 5. 协议与异常

新增 NDJSON 方法：

```json
{"id":1,"method":"capabilities"}
{"id":2,"method":"render_profile_media","params":{"profile":{},"preset":"qq-v2","quality":90}}
```

`profile` 使用真实 Profile API 响应。结果包含 `items`（seq、kind、
content_type、base64 data）和逐页 `failures`。静态页 JPEG、动态页 MP4；
无法导出/缺少素材的页面标记失败，其余页面继续。所有页面失败会在 BOT
显示首个失败原因。MP4 使用上游 qq-v2 预设（默认最长 8 秒、12 fps、
长边上限 1830），不是保证无限时长或完整支持尚未实现的游戏效果。

下载和导出前检查共用渲染库的 `RenderAssetPlan`，静态页、动态页都会从
实际场景收集图片和遮罩，包括普通卡片的信息覆盖层。`card/bg_base_wh`
仅在配方使用它时请求，下载后仍缺失会明确报错。场景解析失败直接作为
该页失败原因；PNG 解码失败的日志会报告具体资源名和解码错误。

每个区服管道串行使用，请求 ID 校验；超时/取消/崩溃时回收子进程，
下个请求重建，避免把前一个用户的响应错误交给后一个用户。
最多 20 页、总媒体不超过 64 MiB，队列容量默认 8。

同一请求内，静态页和动态页分批并行导出，结果仍按 `seq` 排序，失败按页
记录；64 MiB 限制也按此顺序分配。`SEKAI_PROFILE_PAGE_THREADS` 控制同时
导出的页数，取值 1–16，默认取 CPU 可用并行数且最多 4 页。分批执行可限制
同时存活的动画导出器和临时媒体数量。

原生渲染默认启用 `parallel`：静态页、动画图层和动态 SDF 窗口的光栅化
共用一个线程池。`SEKAI_PROFILE_RASTER_THREADS` 控制池大小，取值 1–64，
默认取 CPU 可用并行数且最多 8 个线程；该值在线程池首次使用时读取。
两个变量均可通过启动渲染服务的进程环境设置。

本文件和 `bot_service.py`、Rust 适配必须与部署版本一起保存在 fork 中。
私有配置、字体、图集、玩家 JSON 不要提交到源码仓库。
