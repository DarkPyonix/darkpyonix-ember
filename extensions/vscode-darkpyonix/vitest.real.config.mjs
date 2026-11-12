import { defineConfig } from "vitest/config";

// Against the real DarkPyonix manager (DARKPYONIX_CLI = path of the `darkpyonix` binary).
export default defineConfig({
  test: {
    include: ["test/real/**/*.test.ts"],
    testTimeout: 60000,
    hookTimeout: 60000,
    fileParallelism: false,
  },
});
