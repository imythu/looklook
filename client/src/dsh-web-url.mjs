// 看看（Looklook）的 DSH 插件：把这个 DSH 网页界面的带口令地址写到本文件旁边的 `web-{pid}.json`，
// 看看用它登录正在运行的 DSH（不管是谁启动的、在哪个端口上），远程经看看的通道打开。
//
// 由看看自动安装（见 looklook-client/src/agents.rs 的 “DSH 网页界面插件”），放在 `$DSH_HOME/looklook/`，
// home 层补丁 `$DSH_HOME/cordis.patch.yml` 里看看那一段引用它。地址用官方的 `connection.authenticatedUrl()`
// 取得（`dsh web` 打印 “dsh web: …” 那一行用的也是它），不读签名密钥，也不自己做 Cookie。
// 没有网页界面的 profile（headless、sdk 等）里什么也不做；不打印任何东西。
import { mkdirSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const name = 'looklook-web-url';

export function apply(ctx) {
  // 服务是可选的：顶层声明 inject 的话，没有网页界面的 profile 每次运行都会提示“1 entry did not activate”
  ctx.inject(['connection', 'webServer'], (c) => {
    const dir = fileURLToPath(new URL('.', import.meta.url));
    const file = join(dir, `web-${process.pid}.json`);
    const write = () => {
      try {
        const base = c.get('webApp')?.getAdvertisedUrl?.() ?? c.webServer.getOrigin?.();
        const port = c.webServer.port;
        if (typeof base !== 'string' || !Number.isInteger(port)) return;
        const url = c.connection.authenticatedUrl(base);
        mkdirSync(dir, { recursive: true, mode: 0o700 });
        const tmp = `${file}.tmp`;
        writeFileSync(tmp, JSON.stringify({ v: 1, pid: process.pid, port, url, at: Date.now() }), { mode: 0o600 });
        renameSync(tmp, file);
      } catch {
        // 写不了就算了：看看会退回读自己启动的 dsh 的输出
      }
    };
    // 和 dsh-web-app 打印地址的时机一样：插件全部加载完、开始监听之后
    const settled = c.get('loader')?.await();
    if (settled === undefined) write();
    else settled.then(write, () => {});
    c.effect(() => () => rmSync(file, { force: true }));
  });
}
