import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";
import globals from "globals";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "dist/**",
      "node_modules/**",
      "test-results/**",
      "test-results-production/**",
      "playwright-report/**",
      // 生成物：契约快照与类型由 scripts/dump_openapi.py 统一产出
      "src/engine/contracts/api-types.ts",
    ],
  },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    ...reactHooks.configs.flat.recommended,
    languageOptions: {
      ecmaVersion: 2022,
      globals: { ...globals.browser, ...globals.node },
    },
    plugins: {
      "react-refresh": reactRefresh,
    },
    rules: {
      "react-refresh/only-export-components": [
        "warn",
        { allowConstantExport: true },
      ],
    },
  },
  {
    // shadcn/ui 组件按官方约定同时导出组件与 variants 辅助函数；
    // tests/helpers 是测试 helper（组件与渲染函数混合导出），无需 fast refresh。
    files: ["src/shared/ui/**", "tests/helpers/**"],
    rules: {
      "react-refresh/only-export-components": "off",
    },
  },
);
