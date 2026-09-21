import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// The frontend lives in ui/ so the repo root stays the Cargo workspace root.
export default defineConfig({
  root: 'ui',
  plugins: [react()],
  clearScreen: false,
  server: {
    // Tauri needs a fixed port it can point the webview at; failing loudly beats
    // silently moving to 5174 and leaving the app showing a blank window.
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // Match what the Tauri webview (WebView2 / WKWebView / WebKitGTK) supports.
    target: 'es2022',
    sourcemap: true,
  },
});
