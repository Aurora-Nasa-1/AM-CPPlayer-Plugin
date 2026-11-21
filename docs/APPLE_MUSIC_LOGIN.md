# Apple Music 登录

Apple Music Provider 不接收 Apple ID 和密码，也不会替你登录 Apple 账号。登录时需要
从你自己已登录的 Apple Music 网页会话中复制 `media-user-token`，再交给 CPPlayer。
开发者 token 与它不是同一个凭据；开发者 token 可由 Provider 自动发现，无需手动填写。

## 获取 `media-user-token`

1. 在桌面版 Edge 或 Chrome 打开 `https://music.apple.com/`，登录你自己的 Apple 账号，
   并确认该账号有可用的 Apple Music 订阅。
2. 按 `F12` 打开开发者工具，进入 **Network（网络）**，启用 **Preserve log（保留日志）**。
3. 刷新 Apple Music 页面，或在网页中搜索一首歌，让页面重新发起 API 请求。
4. 在请求列表中找到发往 `amp-api.music.apple.com` 的请求。优先选一个 `/v1/me/`
   开头的请求；如果没有，选其他 Apple Music API 请求。
5. 打开该请求的 **Cookies** 面板，在 **Request Cookies** 中找到
   `media-user-token`，复制它对应的 **Value**。只复制值本身，不要带
   `media-user-token=`、`Cookie:`、其他 Cookie 字段或引号。
6. 如果没有 **Cookies** 面板，检查 **Headers → Request Headers** 中的 `Cookie`：
   找到 `media-user-token=...` 并只复制等号后该字段的值；不要复制整个 Cookie 行。

`media-user-token` 是账号会话凭据，泄露后可能被他人用于访问你的 Apple Music 账号数据。
不要把它发给他人、贴到公开 issue、截图或提交到 Git 仓库。若误泄露，请在 Apple Music
网页退出并重新登录，再按上述步骤获取新值。

## 在 CPPlayer 中登录

1. 打开 **设置 → 音源账号**，切换到 **Apple Music (Rust)**。
2. 选择 **Cookie** 登录方式。
3. 将刚才复制的 token 值粘贴到输入框并提交。这里接受 Apple 的单段 token，也兼容其他音源
   使用的 `name=value; ...` Cookie 格式。
4. 登录状态以音源账号接口校验结果为准。若提示凭据无效，请重新从已登录的网页会话复制；
   不要改填 Apple ID 密码或 `developerToken`。

凭据由 CPPlayer 按音源账号保存，并作为当前 Provider 会话凭据传递；Provider 关闭会话时
会释放内存中的副本。搜索与公开目录浏览不要求该 token；个人资料、个人歌单和会员授权
播放需要有效 token。
