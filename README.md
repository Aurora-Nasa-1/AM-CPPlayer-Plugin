# Apple Music Provider for CPPlayer (Rust)

Apple Music 音源，面向 CPPlayer KMP 的 `kmp-sdk-v1` Provider 契约。
原项目 [gamdl](https://github.com/glomatico/gamdl)（Python + PyO3 下载器）只保留
Apple Music Web API 接口语义；下载、标签、转码、CLI、本地 CDM / 解密与本地库管理
逻辑已移除，仅保留 DRM 信令与 license 交换接口。

## 保留的 API

Provider 只声明并实现以下操作，其余操作由宿主直接判为 `Unsupported`：

| 操作 | tag | 能力 |
|---|---|---|
| `search` | 2 | `tracks.search` |
| `get` | 3 | `tracks.get` |
| `getMany` | 4 | `tracks.getMany` |
| `resolve` | 5 | `tracks.resolve` |
| `resolveDrm` | 168 | `tracks.resolveDrm` |
| `exchangeLicense` | 169 | `tracks.exchangeLicense` |
| `getLyrics` | 10 | `lyrics.get` |
| `searchPlaylists` | 11 | `playlists.search` |
| `getPlaylist` | 12 | `playlists.get` |
| `getPlaylistTracks` | 13 | `playlists.tracks` |
| `getUserPlaylists` | 14 | `playlists.user` |
| `searchAlbums` | 15 | `albums.search` |
| `getAlbum` | 16 | `albums.get` |
| `searchArtists` | 17 | `artists.search` |
| `getArtist` | 18 | `artists.get` |
| `getArtistTracks` | 19 | `artists.tracks` |
| `getArtistAlbums` | 20 | `artists.albums` |
| `getCurrentAccount` | 59 | `accounts.session` |

上游端点均来自 Apple Music Web API（`amp-api.music.apple.com`），
没有重新引入旧插件的端点映射或 JSON 封套。

## 会话输入

Apple Music 登录需要手动提供网页会话中的 `media-user-token`。CPPlayer 的登录入口和
安全注意事项见[登录说明](docs/APPLE_MUSIC_LOGIN.md)。

| 输入 | 格式 |
|---|---|
| `credential` | JSON 对象，或直接给 `media-user-token` 字符串 |
| `settingsJson` | 扁平字符串映射 |

```json
{
  "developerToken": "eyJ...",
  "mediaUserToken": "At...",
  "storefront": "us",
  "language": "en-US"
}
```

`developerToken` 缺省时，会在会话内首次请求时从 Apple 公开网页播放器提取并缓存；
`mediaUserToken` 缺失时，搜索/浏览可用，个人库与账号接口返回 `Authentication`。

`getLyrics` 走 Apple 的目录歌词端点；该端点需要有效的会员授权，未授权或曲目
没有可访问歌词时返回 `NotFound`。

| 设置键 | 默认值 | 说明 |
|---|---|---|
| `storefront` | `us` | 目录地区 |
| `language` | `en-US` | 本地化语言 |
| `apiBase` | `https://amp-api.music.apple.com` | 目录 API |
| `siteBase` | `https://music.apple.com` | 开发者 token 发现来源 |
| `coverSize` | `1200` | 封面边长 |
| `allowPreview` | `true` | 是否允许返回未加密试听资源 |

凭据只存在于会话内存，`close` 后释放。

打包清单声明三个业务音质 key：

| key | targetQuality | 说明 |
|---|---|---|
| `drm-standard` | `standard` | 加密 AAC HLS |
| `drm-lossless` | `lossless` | 加密 ALAC HLS |
| `preview` | `standard` | 未加密试听；显式选择时走 `resolve` |

`actualQuality` 由实际选中的 HLS variant 决定；请求无损但上游只给 AAC 时，
返回 `standard`，不伪装档位。

## 播放能力边界

播放有两条路径，均由 Provider 实现，宿主按平台能力选择：

| 路径 | 操作 | 返回 | 平台 |
|---|---|---|---|
| 加密 HLS | `resolveDrm` + `exchangeLicense` | `ResolvedMedia`（`resource` + `DrmDescriptor`） | Android Media3 / Windows WebView EME（均为 Widevine） |
| 明文试听 | `resolve` | 公开 `attributes.previews` AAC/M4A | 所有平台 |

| 平台 | 解密载体 | key system |
|---|---|---|
| Android | Media3 `FrameworkMediaDrm`（Flick 模式强制切 Media3） | Widevine |
| Windows 桌面 | WebView EME 桥（Edge / Chrome / WebView2 Runtime 内核 + hls.js） | Widevine |
| macOS / iOS | 原生 FairPlay | FairPlay（宿主尚未接入） |

### DRM 路径

`resolveDrm` 会：

1. 把 `library/i.*` 映射到目录曲目 ID；
2. 请求 Apple `/v1/play/assets`，取回 HLS master；
3. 解析 `EXT-X-KEY` / `EXT-X-SESSION-KEY`，按宿主给出的 key system
   优先级选择 Widevine / PlayReady / FairPlay 信令；
4. 按业务音质 key 选择 AAC 或 ALAC variant，返回该 variant 的 media
   playlist 与真实 `actualQuality`；
5. 只返回 `DrmDescriptor` 信令，不生成、不缓存 content key 或 license。

`exchangeLicense` 把宿主 CDM 的 challenge 发到 Apple
`acquireWebPlaybackLicense`，并把返回的 `license` 解码为原始字节。challenge
与 license 不会写入日志、磁盘或错误消息。

如果 manifest 里没有宿主请求的 key system（例如 Apple 只给 FairPlay，而
Android 只支持 Widevine），Provider 返回 `Unsupported`，宿主回退到明文
preview；不会返回一个必然播放失败的加密 URL。

Windows 侧没有系统级 Widevine 接口，宿主改用浏览器内核解密：本地播放页经
`hls.js` 建立 EME 会话，challenge 通过 Provider 的 `exchangeLicense` 换成
license 后原样回传 CDM。Provider 在这条链路上与 Android 完全一致，不需要
感知解密发生在哪里。真机 / 真内核验收步骤见 CPPlayer-KMP 仓库的
`docs/dev/providers/DRM_VERIFICATION.md`。

### 明文试听路径

- 未加密 AAC/M4A，可直接交给宿主播放器；
- `actualQuality` 固定为 `preview`，不把请求档位伪装成实际音质；
- 个人库 ID（`library/i.*`）先解析出目录对应曲目，再取该曲目的试听；
- `allowPreview=false` 时，非 `preview` 请求返回 `Unsupported`；显式请求
  `preview` 仍可用。曲目没有试听时返回 `NotFound`。

DRM 加密流不能进入下载、磁盘流缓存、`StreamLocalizer` 或 `LocalServer`
导出；这些路径只处理明文。

### DRM 链路验收

协议与信令有单元/契约测试，但真实解密必须用 CDM 与 Apple 凭据验证：

```powershell
# 真实凭据 + 本机浏览器内核，跑通 Widevine challenge → license → 解密帧
$env:AM_MEDIA_USER_TOKEN = '<media-user-token>'
$env:AM_DEVELOPER_TOKEN  = '<developer-token>'   # 可留空
cargo run --release --bin drm_e2e -- 1440857781 drm-standard
```

工具会打印 `resolved` / `license` / `decoded frames` / `duration` 四个断言，
不含 challenge 或 license 字节。Android 真机与 FairPlay 的验收步骤见
CPPlayer-KMP 仓库 `docs/dev/providers/DRM_VERIFICATION.md`。

`drm_e2e` 需要满足两个条件：本机有 Edge / Chrome / WebView2 内核（从中取真实
Widevine challenge），以及有效的 Apple Music 订阅 token。缺任何一个都会明确
失败，不会伪造通过。可用 `AM_HLS_JS=<path>` 指定本地 hls.js，否则从 jsDelivr 加载。

Windows 上需要 MSVC 环境；没有全局 `cl.exe` 时用仓库脚本同样的方式加载：

```powershell
cmd /c "`"E:\project\build\VC\Auxiliary\Build\vcvars64.bat`" >nul 2>&1 && cargo run --release --bin drm_e2e -- 1440857781 drm-standard"
```

## 构建

```powershell
# Windows x64（提供 MSVC 环境）
cargo build --release

# 单元测试 + 契约测试
cargo test

# 打包可导入的模块 zip
pwsh -File scripts/package.ps1
```

`scripts/package.ps1` 生成 `dist/apple-music-windows.zip`，结构为：

```text
manifest.json
am_sdk_provider.dll
lib/x86_64/am_sdk_provider.dll
```

包内 `manifest.json` 的 `type` 是 `native`：同一个二进制同时导出 JNI
`NativeSession` 与 C ABI v1（`cpp_*`），CPPlayer 1.9+ 的原生构建按 C ABI
直调。Windows 打包会校验五个 `cpp_*` 符号，缺任何一个都直接失败，不会产出
只能装不能用的包。

其他平台：

```powershell
pwsh -File scripts/package.ps1 -Platform linux -Target x86_64-unknown-linux-gnu
pwsh -File scripts/package.ps1 -Platform macos -Target aarch64-apple-darwin
$env:ANDROID_HOME = '<Android SDK>'
pwsh -File scripts/package.ps1 -Platform android -Target aarch64-linux-android
```

Android 需要 NDK（`ANDROID_NDK_HOME`，或 `ANDROID_HOME/ndk/<version>`）与
`rustup target add aarch64-linux-android`。包内库位于
`lib/arm64-v8a/libam_sdk_provider.so`，`entryPoint` 指向该路径。

## 发布

推送形如 `v1.0.0` 的 tag 会触发
[`.github/workflows/release.yml`](.github/workflows/release.yml)，构建并发布：

- `apple-music-windows.zip`
- `apple-music-linux.zip`
- `apple-music-macos.zip`
- `apple-music-android.zip`
- `SHA256SUMS.txt`

发布使用 GitHub Actions 内置的 `GITHUB_TOKEN` 和 `gh release create`，不依赖
本地凭据。需要手动补发时，可在已登录的 `gh` 环境执行：

```bash
gh release create v1.0.0 dist/*.zip --generate-notes
```

## ABI

同一份二进制导出两套符号。JVM 桌面 / Android 走 JNI，GraalVM Native Image 走 C ABI v1。

| 符号 | 说明 |
|---|---|
| `Java_cp_player_sdk_jni_NativeSession_createNative` | `(providerId, accountId, credential, settingsJson) -> jlong` |
| `Java_cp_player_sdk_jni_NativeSession_invokeNative` | `(handle, operation, request) -> byte[]` |
| `Java_cp_player_sdk_jni_NativeSession_cancelNative` | 线程安全取消 |
| `Java_cp_player_sdk_jni_NativeSession_closeNative` | 排空后释放 |
| `cpp_abi_version` | 返回 `1` |
| `cpp_create` / `cpp_invoke` / `cpp_cancel` / `cpp_close` | C ABI v1 |

响应为 `[0][payload]` 或 `[1][category][message]`，编解码与契约布局均为位置式小端格式。

## 契约维护

`src/models.rs` 与 `src/operations.rs` 由 `tools/generate_sdk.mjs` 生成，不要在
手写代码里复制字段顺序：

```powershell
node tools/generate_sdk.mjs
```

脚本默认读取 `../CPPlayer-KMP/provider-contract`，也可用
`CPPLAYER_CONTRACT_DIR` 指定。

## 许可证

见 [LICENSE](LICENSE)。原 gamdl 项目同样以 MIT 许可发布。
