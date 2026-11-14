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
| 加密 HLS | `resolveDrm` + `exchangeLicense` | `ResolvedMedia`（`resource` + `DrmDescriptor`） | Android Media3 / Widevine |
| 明文试听 | `resolve` | 公开 `attributes.previews` AAC/M4A | 所有平台 |

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

### 明文试听路径

- 未加密 AAC/M4A，可直接交给宿主播放器；
- `actualQuality` 固定为 `preview`，不把请求档位伪装成实际音质；
- 个人库 ID（`library/i.*`）先解析出目录对应曲目，再取该曲目的试听；
- `allowPreview=false` 时，非 `preview` 请求返回 `Unsupported`；显式请求
  `preview` 仍可用。曲目没有试听时返回 `NotFound`。

DRM 加密流不能进入下载、磁盘流缓存、`StreamLocalizer` 或 `LocalServer`
导出；这些路径只处理明文。

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

其他平台：

```powershell
pwsh -File scripts/package.ps1 -Platform linux -Target x86_64-unknown-linux-gnu
pwsh -File scripts/package.ps1 -Platform android -Target aarch64-linux-android
```

## 发布

推送形如 `v1.0.0` 的 tag 会触发
[`.github/workflows/release.yml`](.github/workflows/release.yml)，构建并发布：

- `apple-music-windows.zip`
- `apple-music-linux.zip`
- `apple-music-macos.zip`
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
