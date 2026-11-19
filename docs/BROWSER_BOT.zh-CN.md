# BOT 浏览器与原生组合渲染部署

组合渲染使用浏览器处理静态名片，使用原生程序导出动态名片。相关代码以 `BROWSER-01` 标注。
现有 `/render` 请求、鉴权和返回的图片/视频结构不变，NewLunabot 的
`profile_renderer.py` 和 `modules/profile.py` 无需修改。

组合部署设置 `backend: "browser"`；该值包含动态页交给原生程序导出的行为，
不需要额外配置第三种后端。`backend: "native"` 表示全部页面由原生程序处理。
示例配置和省略 `backend` 时的代码默认值仍为 `native`，部署组合模式时需要明确修改。

## 行为

- 静态页面：常驻 Chromium + 当前项目的 Rust/WASM/WebGL2 SDK，输出 JPEG。
- 动态页面：共享 Rust/TMP 解析器先识别候选页，跳过浏览器字体、SDF、图片和 GPU 场景准备，
  交给已有原生程序导出；原生预检确认有可见动画时输出 MP4，
  判定无需动画时仍可输出静态 JPEG。动态导出的绘制、帧合成和 libx264 编码使用 CPU。
- 调度顺序（`PERF-BROWSER-02`）：浏览器启动后先对整批页面做轻量分类，再并行运行原生
  动态导出与浏览器静态渲染，最后合并结果。结果按 `seq` 排序，共用 64 MiB 返回预算；
  服务返回整批结果后，BOT 再发送媒体。分类失败时仍尝试原完整场景流程；该流程发现的
  动态页在浏览器处理后补交原生。请求超时或取消时会取消并等待两个后端任务退出，再关闭资源。
- 每个区服按需启动一份 Chromium，默认两个常驻页面。每个页面持有自己的 SDK Worker、
  字体、解码图片和字形缓存，请求完成后释放场景，不保留玩家场景。浏览器文字画图集上限
  为每 Worker 16 页，细节与部署建议见[采样修复说明](browser-sdf-quality.zh-CN.md)。
- 字体和 MasterData 直接读取现有本机目录；图片通过现有 BOT 内部素材接口获取。
  BOT 令牌由 Python 代理持有，不发送到网页 JavaScript。
- 默认检查实际 WebGL renderer，拒绝 SwiftShader 等软件渲染或无法确认硬件的环境。
- 原生环境变量只影响原生动态导出和纯原生模式，不控制浏览器页面并发。
- 浏览器版与原生版的字形回退、边缘像素和 JPEG 编码可能有所不同，部署更新后应检查输出。

静态文字画的大字号字形现在默认按实际放大倍率提高 SDF 采样分辨率，修复低分辨率字形被
大幅放大时的发尖钝化和曲线细节损失。代码以 `FIX-BROWSER-SDF-01` 标注，构建部署、缓存
与适用范围见[浏览器文字画采样修复](browser-sdf-quality.zh-CN.md)。此修复需要更新完整浏览器 SDK。

2026-11-17 的组合渲染优化需要同时构建 `build-bot.ps1` 和
`build-browser-bot.ps1 -FromSource`。部署时更新原生 exe 与配套 DLL、完整
`runtime/browser-sdk/dist/`、`bot_browser.py`、`browser_bot/bridge.mjs`，停止服务后统一替换。
BOT 项目和现有配置无需修改。代码标记及耗时字段见
[性能优化记录](native-performance.zh-CN.md)。

## 1. 在源码机器准备浏览器 SDK

原 `build-bot.ps1` 只生成原生程序，不能生成浏览器 SDK。
浏览器 SDK 需要完整配套的 JS、WASM 和 FreeType 许可文件，不能只复制一个 WASM。

有与当前源码匹配的 browser 发布 ZIP（内含 `dist/`）时，可直接导入，无需编译：

```powershell
.\build-browser-bot.ps1 -SdkArchive 'D:\Downloads\sekai-custom-profile-sdk-browser-0.8.0.zip'
```

已有完整的浏览器 `dist` 目录时：

```powershell
.\build-browser-bot.ps1 -SdkDirectory 'D:\SDK\dist'
```

如果没有产物，在本机安装 Node.js 24、npm 和 Docker Desktop，启动 Docker 的 Linux
容器引擎。Windows 安装 Docker Desktop 时使用 WSL 2 后端；若提示缺少“虚拟机平台”，
在管理员 PowerShell 执行 `wsl --install --no-distribution`，完成后重启再继续安装。

打开 Docker Desktop，等待引擎就绪。`docker version` 应同时显示 Client 和 Server，
然后在仓库根目录运行：

```powershell
.\build-browser-bot.ps1 -FromSource
```

该模式用仓库已有 Dockerfile 构建匹配的 Rust/WASM/FreeType，再编译 TypeScript。
首次需要联网拉取构建依赖。结果统一放在：

```text
runtime/browser-sdk/dist/
```

部署机只需要产物，不需要 Docker、Rust 或 Node.js。

构建时若拉取 `emscripten/emsdk:4.0.10` 超时，应检查 Docker Desktop 的网络与代理设置。
PowerShell 中的代理环境变量不一定会被 Docker 后台采用；需要代理时，在 Docker Desktop
的 Proxies 设置中填写代理软件实际提供的 HTTP/Mixed 地址和端口。可以先运行
`docker pull emscripten/emsdk:4.0.10` 检查镜像下载，再重新构建。

## 2. 部署文件与运行依赖

保持现有 exe、DLL、字体、图集和配置路径，补充或更新：

```text
渲染服务目录/
  bot_service.py
  bot_browser.py
  browser_bot/
    bridge.mjs
  runtime/
    browser-sdk/
      dist/                      完整复制，包含所有子目录
  bot_service.json               修改下列字段
```

原生程序及其 DLL、字体和图集目录同样属于组合部署的一部分，参见
[BOT 本地名片渲染部署](BOT_DEPLOYMENT.zh-CN.md)。

使用启动服务的同一个 Python 环境，在部署机安装：

```powershell
python -m pip install aiohttp playwright
python -m playwright install chromium --no-shell
```

使用完整 Chromium 的新 headless 模式，不使用 headless shell。
安装命令和运行服务应使用同一个 Windows 用户，避免找不到该用户的浏览器安装。

## 3. 配置组合模式并启动

在现有 `bot_service.json` 中添加或更新以下字段；原来的 token、bot_root、
bot_url、字体目录、原生 exe 路径等继续保留：

```json
{
  "backend": "browser",
  "browser_sdk_dir": "runtime/browser-sdk/dist",
  "browser_page_threads": 2,
  "browser_keepalive_regions": ["cn", "jp"],
  "browser_channel": "chromium",
  "browser_headless": true,
  "browser_require_hardware": true
}
```

`browser_sdk_dir` 相对配置文件所在目录。`browser_page_threads` 支持 1～8，默认 2；
它与原生的 `SEKAI_PROFILE_PAGE_THREADS`、`SEKAI_PROFILE_RASTER_THREADS` 无关。
`browser_keepalive_regions` 用区服列表控制常驻 browser，例如 `["cn", "jp"]`；默认 `[]`，
未列出的区服仍在收到请求时启动。列出的区服会在服务启动时预热，并每 60 秒检查 masterdata、
字体和图集签名；发生变化或 Chromium 断开时会自动重建。预热失败会记录错误并继续重试，
不会阻止 HTTP 服务启动。每个常驻区服会持续占用 Chromium 与 GPU/系统内存。
JPEG 画质继续读取现有 `image_quality`。浏览器 JPEG 的质量刻度不保证与 libjpeg 完全等效。

停止原服务，再按原命令启动：

```powershell
python .\bot_service.py --config .\bot_service.json
```

地址、端口和 token 保持不变时，BOT 不用改配置，继续使用原来的 `/cp` 命令。
部署后先用 `/cp 1` 检查单页，再用 `/cp` 检查整批静态与动态媒体。

## 4. GPU 状态与运行检查

第一次渲染后，日志应出现类似：

```text
[cn] browser slot=0 GPU={'unmaskedRenderer': 'ANGLE (... AMD Radeon RX 580 ... Direct3D11 ...)'}
```

也可以查看状态：

```powershell
$health = Invoke-RestMethod 'http://127.0.0.1:45557/health'
$health.backend
$health.workers.cn | ConvertTo-Json -Depth 6
```

尚未收到该区服的请求时，`started=false`、`gpu=[]` 是正常的按需启动状态。
如果 GPU 包含 SwiftShader、Microsoft Basic Render Driver 等，默认明确报错，
避免把软件渲染当成显卡加速结果。
排查时可以将 `browser_headless` 设为 `false`，在部署机交互桌面启动服务观察浏览器。
只有明确要测软件渲染时才关闭 `browser_require_hardware`，该结果不能用于判断 RX580 收益。

部署日志已确认 Windows 上的 RX580 2048SP 通过 ANGLE/Direct3D11 执行 WebGL2 绘制。
这项检查确认的是浏览器绘制使用 GPU，不表示动态页已启用硬件视频编码。

## 5. 耗时与运行维护

日志按页输出：

- `prepare_ms`：SDK 场景准备，包含需要的排版、字形生成、素材加载等。
- `draw_copy_ms`：提交绘制并复制到导出画布，不是独立的纯 GPU 计时。
- `encode_transfer_ms`：JPEG 编码和浏览器内转成 Base64；也可能包含等待绘制完成。
- `total_ms`：该页浏览器处理时间，不包括等待浏览器页面空闲和 Python/Playwright 外层传输。

HTTP 返回的 `compute_ms` 覆盖获得执行权限后的处理时间，`total_ms` 还包含排队等待。
处理时间包含首次浏览器启动，以及需要时的原生进程启动和动态导出。
浏览器静态结果增加 `backend: "browser"` 和 `timing`；交给原生导出的结果标为
`backend: "native-animation"`，不包含浏览器的逐页计时。
不要把单页绘制耗时与整个 profile 导出耗时直接比较。

重启后先渲染一次，再连续测相同资料。比较原生和浏览器时保持分辨率、资料与画质配置一致，
并同时查看输出大小和画面。浏览器和原生进程按需启动并常驻；MasterData 或字体更新时会
重新加载相关状态。首次请求或更新后的请求可能明显慢于缓存已建立时的请求。

部署反馈中，一份包含复杂静态页和两张动态页的资料，整批返回时间由原生模式的五十多秒
降至组合模式的约二十秒。这是单份资料的运行反馈；耗时还受场景复杂度、缓存、素材下载和
动态导出影响，不作为固定性能指标。

`Ctrl+C` 可用于停止服务，执行中的请求可能中断。关闭时若出现
`Browser.close: Connection closed while reading from the driver`，表示清理时驱动连接已断开；
不代表字体或玩家资料损坏。重启后检查 `/health`；若仍有本服务启动的 Chromium 或
`sekai-profile.exe` 残留，确认归属后再结束对应进程。

### 切换为纯原生模式

修改配置并重启服务：

```json
{ "backend": "native" }
```

## 代码位置与检查

- `bot_service.py`：后端选择，保持 HTTP 契约；原生模式无需 Playwright。
- `bot_browser.py`：常驻浏览器池、本机资源桥接、GPU 检查、逐页调度和原生动态导出。
- `browser_bot/bridge.mjs`：浏览器 SDK 初始化、场景生命周期和 JPEG 导出。
- `build-browser-bot.ps1`：浏览器产物构建或导入，不影响原生构建脚本。
- `tests/test_bot_browser.py`、`tests/browser_bridge.test.mjs`：协议、资源、清理及浏览器桥接检查。

本机可执行不依赖 Chromium 或 Rust 编译的检查：

```powershell
$env:PYTHONDONTWRITEBYTECODE = '1'
python -m unittest discover -s tests -p 'test_bot_browser.py' -v
node --test tests/browser_bridge.test.mjs
```

这些检查使用模拟浏览器，不能替代真实 WASM/WebGL2/GPU 的集成验证。
