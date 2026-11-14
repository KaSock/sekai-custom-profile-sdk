# 原生渲染性能优化记录

本次修改作用于 Rust 原生 CPU 渲染及服务编码路径。源码中的 `PERF-01` 至
`PERF-05` 注释对应下表。没有进行编译、测试执行或性能测量，速度收益和像素一致性
仍需在部署机验证。

部署机 Xeon E5-2680 v4 支持 AVX2/FMA，不支持 AVX-512。默认 `Auto` 会在该机
选择 AVX2；程序仍在运行时检查 CPU 和操作系统支持，其他主机可以自动选择
AVX-512 或标量路径。继续使用现有 `build-bot.ps1` 构建即可。

## 优化位置

以下路径相对仓库根目录。函数名用于定位代码段，不依赖后续变动的行号。

| 标记 | 代码段 | 修改及作用 |
| --- | --- | --- |
| PERF-01 | `crates/sekai-profile/src/assets.rs`：`DecodedAsset`、`DecodedLru`、`AssetStore::get_decoded` | 按字节预算缓存解码、预乘后的像素及 SHA-256；命中时复用 `Arc`。 |
| PERF-01 | `crates/sekai-profile/src/profile_compositor/commands.rs`：`load_source_fallback_object`；`profile_compositor/mod.rs`：`OwnedRenderObject` | 缺少 mmap 纹理时复用缓存像素和哈希，避免每条命令重新解码、计算哈希、复制整张图片。 |
| PERF-02 | `crates/sekai-profile/src/text/mod.rs`：`with_page_text_layout_cache`、`capture_text_sdf_from_affine`；`renderer/surface.rs`：`render_full_card` | 单页渲染内复用文字排版；实时 EDT 需求采集与最终绘制共享布局，绘制时重新应用当前变换。 |
| PERF-02 | `crates/sekai-profile/src/profile_compositor/commands.rs`：`ProfileSceneIndex`、`render_image_commands_indexed`；`scene.rs`：`render_authored_profile_indexed` | 每个输出表面只建一次图层映射和元素命令索引；元素绘制读取自身有序命令列表。 |
| PERF-02 | `crates/sekai-profile/src/renderer/surface.rs`：`render_ordered_sdf_surface`；`renderer/authored.rs`：`render_authored_image_into_pixels` | 将索引接入原生页渲染。动画发生画布原点平移时使用平移后的场景，不复用原矩阵索引。 |
| PERF-03 | `crates/sekai-profile/src/render_config.rs`：`resolve_features`、`sdf_avx2_available`；`lib.rs` | 增加 `Avx2` 执行器和运行时 CPU 分派，保留原 AVX-512、标量路径。 |
| PERF-03 | `crates/sekai-profile/src/sdf/tile/execute/avx2.rs`：`shade_blend_packet`；`execute.rs`：`execute_avx2_*`、`execute_packet_rows` | 每包处理 8 个片元的坐标、文字双线性插值、文字着色及 FP32 混合；尾部使用标量。 |
| PERF-03 | `crates/sekai-profile/src/sdf/tile/native_execute.rs`：`execute_native_f32`；`profile_compositor/semantic_text.rs`：执行器分支 | 将 AVX2 接入原生并行行带及组件文字渲染。 |
| PERF-03 | `crates/sekai-profile/src/profile_compositor/avx2.rs`：`local_coordinates`、`blend_rgba8_packet`；`raster.rs`、`blend.rs` | 图片坐标用 8 路 AVX2/FMA，RGBA8 混合用 AVX2 整数运算，覆盖六种现有混合模式及不透明隔离组归并。 |
| PERF-03 | `crates/sekai-profile-cli/src/main.rs`：`Executor`、`service_executor`；`serve.rs`、`bot_media.rs` | CLI 支持 `--executor avx2`；服务读取执行器环境变量；能力响应增加 AVX2/AVX-512 检测结果。 |
| PERF-04 | `crates/sekai-profile/src/codec/png.rs`：`encode_rgba_fast`、`fast_scanlines`、`encode_rgba_mode`；`output.rs`：`RenderedPage::to_png` | 最终页 PNG 默认只比较 Sub/Up 行滤波，执行一次 DEFLATE；原模式比较两份完整压缩结果。 |
| PERF-05 | `crates/sekai-profile-cli/src/main.rs`：`render_page`、`encode_jpeg` | 服务改用消耗页面的编码接口，将页面 RGBA 缓冲区交还渲染器，供后续请求复用。 |

### 缓存与像素边界

- 解码缓存独立于原有压缩素材缓存。默认额外预算等于 `AssetStore::new` 的预算；
  例如原预算 256 MiB 时，解码缓存也默认 256 MiB。缓存预算计入像素和持有的
  压缩数据，不包含容器元数据；正在渲染的 `Arc` 引用可延长被淘汰数据的寿命。
- 素材命中须匹配当前压缩数据的 `Arc` 版本。同名素材被替换后不会命中旧像素；
  原有磁盘素材缓存的更新规则保持原样。并发首次请求可能各自解码，后续请求复用。
- 文字缓存只存在于当前线程的一次完整页渲染内，不跨请求保存，不缓存画布变换。
  已提前准备 EDT 的批量或动画路径不一定能省掉同样多的排版工作。
- 命令索引保持原顺序和过滤条件，包括隔离组边界；不重排透明元素。
- AVX2 图集取样保留逐点检查；形状 SDF 着色仍调用原标量函数，再向量混合。
  图片纹理读取、遮罩、光照材质计算仍走现有取样逻辑；带 tint 的图片混合、
  半透明隔离组归并和组件语义形状仍走原标量逻辑。
- AVX2 的 FP32 运算顺序按标量参考路径实现；是否像素一致尚未通过执行测试确认。
- PNG 两种模式均为无损编码，输出文件字节和体积可以不同。JPEG 不受 PNG 开关影响。

## 部署开关

在启动渲染服务之前设置环境变量，并重启服务。现有 `bot_service.py` 会将环境传给
Rust 子进程，无需修改 BOT 项目。

| 变量 | 默认值 | 可选值与用途 |
| --- | --- | --- |
| `SEKAI_PROFILE_EXECUTOR` | `auto` | 服务请求选用 `auto`、`scalar`、`avx2` 或 `simd`；不支持的显式执行器会报错。 |
| `SEKAI_PROFILE_DECODED_CACHE_MB` | 原压缩素材缓存预算 | 非负 MiB；`0` 关闭解码缓存的保留，可用于比较冷、热请求。 |
| `SEKAI_PROFILE_PNG_MODE` | 快速模式 | `small` 恢复原 PNG 输出算法；其他值使用快速模式。 |
| `SEKAI_PROFILE_RASTER_THREADS` | 可用 CPU 并行度，最多 8 | 原有开关；比较执行器时保持线程数一致。 |

PowerShell 示例：

```powershell
$env:SEKAI_PROFILE_EXECUTOR = 'avx2'
$env:SEKAI_PROFILE_DECODED_CACHE_MB = '256'
$env:SEKAI_PROFILE_PNG_MODE = 'fast'
# 随后按现有方式启动渲染服务。
```

恢复自动分派可以设置 `$env:SEKAI_PROFILE_EXECUTOR = 'auto'`。
`--executor` 仅用于 CLI `render` 命令；`serve` / BOT 使用上表的环境变量。
调用 NDJSON `capabilities` 可查看 `sdf_avx2` 和 `sdf_avx512`，它们表示可用能力。
原生渲染遥测中的 `executor` 表示该次实际选择的执行器。

## 建议验证顺序

1. 用现有 `build-bot.ps1` 构建，复制到部署机，先以默认设置运行。
2. 同一份资料、同一页、相同输出格式与 JPEG 质量，各做首次请求和预热后的重复请求。
   分别记录耗时、输出字节数和进程内存。素材下载耗时应单独观察。
3. 固定线程数和其余开关，比较 `scalar` 与 `avx2`；检查多文字、旋转图片、遮罩、
   半透明、描边和动画页面。应比较解码后的像素，PNG 文件哈希可能随编码模式变化。
4. 固定执行器，分别比较缓存预算 `0` / `256`、PNG 的 `small` / `fast`。
   使用 JPEG 的 BOT 请求主要观察解码缓存、排版、AVX2 和缓冲区复用收益。
5. 并发比较使用相同请求数量和并发度；双路 Xeon 的线程数最优值需实测，不能用
   本机 CPU 或 SIMD 向量宽度推算整页加速比例。

已补充但未执行的回归测试：

- `assets.rs`：解码缓存复用、素材替换、损坏素材、预算淘汰和关闭缓存。
- `text/tests.rs`：页内复用布局、重新应用变换、跨请求失效。
- `render_config.rs`：模拟 CPU 能力下的自动和显式执行器选择。
- `profile_compositor/avx2.rs`：全部字节乘积舍入、六种混合模式、活动掩码、
  不对齐地址和 1～16 像素尾部。
- `profile_compositor/tests.rs`：索引与原过滤路径，以及 AVX2 在遮罩、裁剪、tint、
  隔离组中的像素对照。
- `sdf/tile/tests.rs`：非均匀图集、混合文字/形状、仿射、裁剪、尾部、并行行带、
  不支持 CPU 和输出尺寸错误。
- `codec/png/tests.rs`：快速 PNG 的含 alpha 像素往返和无效尺寸。

本轮只执行源码格式化和静态差异检查，没有运行 `cargo build`、`cargo check`、
`cargo test` 或 `build-bot.ps1`。

## FIX-EDT-01：并发补字后实时图集编号失效

2026-11-14 排查 `full-card SDF plan build: command 4 references missing atlas set 4 page 4`
时发现：BOT 并行导出页面，其他页面可能在实时 EDT 批次准备完成后首次发布 fallback
字体图集。运行时图集编号由“当前文字图集数 + 是否安装形状图集”决定；批次保存的
旧编号会与新 fallback 字体或移动后的形状图集冲突，造成缺页或错误取样。

- `renderer/capture.rs::map_captured_sdf_commands`：使用本次绘制所持图集快照计算实时
  图集编号；复用批次内的页号和像素。只在确实需要生成字形时检查字体文件。
- `renderer/realtime_edt.rs::RealtimeEdtPreparedEntry`：移除跨图集快照保存的 `atlas_set`。
- `renderer/tests.rs::prepared_realtime_edt_pages_follow_the_current_atlas_registry`：
  用合成图集模拟批次准备后新增 fallback，展示旧命令的 set 4 / page 4 缺页，验证重新
  映射后的计划可构建；同时覆盖注册表未变化和替换已有 fallback 的情况。
- `sdf/tile/atlas_source.rs::RuntimeTextSdfPage::solid_test_page`：仅测试构建使用的合成页。

此缺陷在命令规划阶段触发，与 AVX2 指令支持无关。源码中存在上述并发触发路径，
但未运行玩家资料复现、编译或回归测试；本次只做格式化与静态差异检查。
