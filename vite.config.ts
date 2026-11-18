import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: "localhost",
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    // WebView iOS 14 (WKWebView) e Android 7+ (System WebView): niente sintassi
    // che il dispositivo non sa eseguire.
    target: ["safari14", "chrome87"],
    sourcemap: false,
  },
});
