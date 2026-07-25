import { resolve } from "node:path";
import { copyFileSync, mkdirSync } from "node:fs";
import { defineConfig } from "vite";

export default defineConfig({
  plugins: [
    {
      name: "sites-static-worker",
      closeBundle() {
        const serverDirectory = resolve(import.meta.dirname, "dist/server");
        mkdirSync(serverDirectory, { recursive: true });
        copyFileSync(
          resolve(import.meta.dirname, "worker.js"),
          resolve(serverDirectory, "index.js"),
        );
      },
    },
  ],
  build: {
    rollupOptions: {
      input: {
        home: resolve(import.meta.dirname, "index.html"),
        privacy: resolve(import.meta.dirname, "privacy/index.html"),
        terms: resolve(import.meta.dirname, "terms/index.html"),
      },
    },
  },
});
