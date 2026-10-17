# wry（看看补丁版）

上游：[wry 0.57.0](https://crates.io/crates/wry/0.57.0)（MIT / Apache-2.0，见 `LICENSE-MIT`、`LICENSE-APACHE`），只复制了编译需要的文件。
客户端 `Cargo.toml` 的 `[patch.crates-io]` 指向这里。改动只在 `src/wkwebview/`（macOS），都标了 `looklook:` 注释。

macOS 上 wry 在 WebKit 的回调里对 `WKWebView.URL()`、`NSURLRequest.URL()` 直接 `unwrap()`。导航失败或被下一次导航取消后
地址会短暂为空，这时 panic 发生在 Objective-C 回调里，不能展开，应用直接崩溃（错误记录里是
`panic in a function that cannot unwind`，线程 main）。看看的用户在打开 DSH、点终端“新窗口打开”时遇到过
（上游 [wry#1752](https://github.com/tauri-apps/wry/issues/1752)，0.57.0 还没修）。

1. `mod.rs` 的 `url_from_webview`：地址为空时返回错误（每次页面开始 / 完成加载都会调用它）。
2. `navigation.rs` 的导航决定：请求没有地址时取消这次导航。
3. `class/wry_web_view_ui_delegate.rs` 的新窗口回调：请求没有地址、发起页面不在窗口里时不开新窗口；窗口不在任何屏幕上时按窗口本身的位置算。
4. `class/wry_web_view_delegate.rs` 的 IPC 消息、`download.rs` 的下载回调：没有地址时忽略 / 用空地址，不 panic。

升级 Tauri / wry 时，把这些改动重新合进去，或者确认上游已经有等价的修复后删掉这个目录和 `[patch.crates-io]` 里的这一行。
