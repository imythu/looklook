import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// 构建产物写入 static/，由 Rust 编进可执行文件（rust-embed）。
// 开发时 `npm run dev` 把 /api、/i、/fonts、/static 转给正在运行的客户端（默认 127.0.0.1:1234）。
const target = process.env.LOOKLOOK_DEV_TARGET || 'http://127.0.0.1:1234';

// Git checks symlinks out as small text files on many Windows installations.
// Read the shared originals for both dev and build, so tray/browser icons stay valid.
const icons = [
  ['apple-touch-icon.png', 'apple-touch-icon.png', 'image/png'],
  ['favicon.ico', 'favicon.ico', 'image/x-icon'],
  ['favicon.svg', 'icon.svg', 'image/svg+xml'],
].map(([fileName, source, mime]) => ({ fileName, mime, source: fileURLToPath(new URL(`../assets/icon-client/${source}`, import.meta.url)) }));
const sharedIcons = {
  name: 'looklook-shared-icons',
  generateBundle() {
    for (const icon of icons) this.emitFile({ type: 'asset', fileName: icon.fileName, source: readFileSync(icon.source) });
  },
  configureServer(server) {
    server.middlewares.use((req, res, next) => {
      const icon = icons.find((i) => `/${i.fileName}` === req.url?.split('?')[0]);
      if (!icon) return next();
      res.setHeader('Content-Type', icon.mime);
      res.end(readFileSync(icon.source));
    });
  },
};

export default defineConfig({
  publicDir: false,
  plugins: [react(), sharedIcons],
  build: { outDir: 'static', emptyOutDir: true },
  server: {
    proxy: Object.fromEntries(['/api', '/i', '/fonts', '/static'].map((p) => [p, { target, ws: true, changeOrigin: false }])),
  },
});
