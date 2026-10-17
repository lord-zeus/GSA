import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';
import { fileURLToPath, URL } from 'node:url';
// Dev server proxies /api + /healthz to the Rust API on :8080,
// so the Vue app calls same-origin paths and CORS is irrelevant in dev.
export default defineConfig({
    plugins: [vue()],
    resolve: {
        alias: {
            '@': fileURLToPath(new URL('./src', import.meta.url)),
        },
    },
    server: {
        port: 5173,
        proxy: {
            '/api': { target: 'http://127.0.0.1:8080', changeOrigin: true },
            '/healthz': { target: 'http://127.0.0.1:8080', changeOrigin: true },
        },
    },
    build: {
        outDir: 'dist',
        emptyOutDir: true,
    },
});
