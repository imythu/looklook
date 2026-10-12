# rathole（看看补丁版）

上游：[rathole 0.5.0](https://crates.io/crates/rathole/0.5.0)（Apache-2.0，见 `LICENSE`），只用客户端与 noise 传输。
客户端 `Cargo.toml` 的 `[patch.crates-io]` 指向这里。改动只在 `src/client.rs`，都标了 `looklook:` 注释：

1. **每条数据通道重新解析中继地址**（`fresh_addr`）。上游只在建立控制通道时解析一次，之后所有数据通道都连这个地址。
   电脑开着 Clash/Surge 的 TUN（fake-ip）时，这个 198.18.x.x 的假地址可能在几个小时后失效：控制通道还连着，
   新的数据通道却一条也建不起来，中转那边的连接池耗尽，浏览器请求一直等待（设备显示在线）。
2. **每次连接尝试限时**：数据通道 5 秒（`CONNECT_ATTEMPT_TIMEOUT`），整体重试预算从 10 秒放宽到 15 秒；控制通道 10 秒
   （`CONTROL_CONNECT_TIMEOUT`）。上游不限时，一次丢包的连接要等系统超时（macOS 75 秒），重试预算第一次就用光，
   这条数据通道就永久丢了（rathole 服务端不会补发）。

升级上游时，把这两处改动重新合进去，或者确认上游已经有等价的修复后删掉这个目录和 `[patch.crates-io]`。
