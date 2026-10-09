# L2 技术设计：只读界面点开成员后仍能看到工作区

版本：v0.1，2026-10-09；状态：Draft。对应 [L1 v0.1](product.md)。

仍由 `atelier --workspace <路径> view` 在 `127.0.0.1` 上提供只读页面。不调用会写入或创建 `runtime.lock` 的打开路径。

- 成员链接是 `GET /?worker=<id>`，不再带 `#latest`。页面锚点 `id="latest"` 仍标在最后一条上。
- 有消息时，脚本按窗口剩余高度限制 `#messages` 的 `max-height`，再把 `scrollTop` 设为 `scrollHeight`。不调用 `scrollIntoView`，因此不滚动窗口，最后一条仍落在窗口内。
- 窄于 720px 时，主区域改为一列。`code` 允许在任意位置换行，避免标识把页面撑出横向滚动。
- 选中成员但消息为空时，保留「0 条」，并显示 L1 规定的那句说明。
- `NotFound` 以 `text/html` 返回，正文是错误说明加上 `href="/"` 的「返回工作区」。找不到成员与未知路径使用 L1 的两句中文。其它错误仍为纯文本。
