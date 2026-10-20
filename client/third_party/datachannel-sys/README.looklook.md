# datachannel-sys（看看补丁版）

上游：[datachannel-sys 0.23.0+0.23.2](https://crates.io/crates/datachannel-sys/0.23.0+0.23.2)（MPL-2.0，见 `LICENSE`），
内含 libdatachannel 0.23.2 及其依赖 libjuice、usrsctp、plog（各自的许可证在对应目录）。
远程控制台走 WebRTC 端到端连接用它；客户端 `Cargo.toml` 的 `[patch.crates-io]` 指向这里，
上层的 `datachannel` crate 仍用 crates.io 上的版本，开 `vendored` 特性（OpenSSL 也静态编进来）。

## 改动

代码里都标了 `looklook:` 注释，对上游的完整差异在 `patches/`：

1. **libjuice：ICE 角色平决值为 0 时回 400**（`libdatachannel/deps/libjuice/src/stun.{h,c}`、`agent.c`，
   `patches/libjuice-ice-role-tiebreaker-zero.patch`）。Chrome 经 TURN/TCP 连进来时 ICE-CONTROLLING 的平决值是 0，
   libjuice 用“值为 0”表示“没有这个属性”，于是把请求当成缺少角色属性、回 400，DTLS 握手一直超时。
   改成单独记录属性是否存在。上游 issue：libjuice#355。
2. **libjuice：本机候选地址**（`libdatachannel/deps/libjuice/src/udp.c`，`patches/libjuice-host-candidates.patch`）。
   本机候选地址最多 9 个，装了 Docker、虚拟机或开着 Clash/mihomo TUN 的电脑上，虚拟网卡占满了名额，真正的局域网地址被挤掉，
   同一局域网的两台设备只能绕公网或中转。改成跳过虚拟网卡（docker、br-、veth、utun/tun/tap、Meta、clash 等）和
   Clash 的 fake-ip / TUN 地址（198.18.0.0/15），并把私有 IPv4 排在最前。
3. **构建**（`build.rs`，`patches/datachannel-sys-build.patch`）：
   - 交叉编译时关掉 CMake 的 pkg-config 查找，否则会用宿主机的 `openssl.pc`，链接时多出 `-lz`；
   - 目标是 Windows 时补链接静态 OpenSSL 和 libjuice 需要的系统库（按 `CARGO_CFG_TARGET_OS` 判断，不能用 `cfg!`）；
   - 打开 `NO_TESTS`，因为下面删掉了测试目录。

## 删掉的目录

只为减小体积，不影响构建（`NO_MEDIA`、`NO_EXAMPLES`、`NO_TESTS` 都已打开）：
`libdatachannel/{test,pages,examples}`、`deps/plog/{.circleci,library.json}`、`deps/libsrtp`（只有媒体功能用，其中模糊测试语料就有 142 MB）、`deps/json`、
`deps/usrsctp/{fuzzer,programs}`、`deps/libjuice/{test,fuzzer}`、`deps/plog/{test,samples,doc}`。

## 升级

拿新版 datachannel-sys 重新做一份副本，按上面的列表删目录，再打 `patches/` 里的补丁；上游已经修复的就删掉对应补丁。
补丁都合进上游后，删掉这个目录和 `[patch.crates-io]` 里的那一行。
