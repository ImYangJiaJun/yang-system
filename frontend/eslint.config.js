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
    languageOptions: {
      ecmaVersion: 2022,
      globals: { ...globals.browser, ...globals.node },
    },
    // `reactHooks.configs.flat.recommended` 只有 plugins / rules 两个键；必须在对象
    // 字面量里显式展开它们——此前的写法把它 spread 在前、再用同名的 plugins/rules
    // 字面量覆盖在后，按 JS 语义后者胜出，react-hooks 插件与 16 条规则被整体丢弃。
    plugins: {
      ...reactHooks.configs.flat.recommended.plugins,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.flat.recommended.rules,
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
  {
    // 逐条显式关闭的 react-hooks 规则（审计 R2-H5）。这些规则在重新启用整套
    // recommended 后各有 1~9 处命中，逐一核对后的处置如下；**不是**为了图省事整体
    // 关掉插件——其余 12 条规则（含 rules-of-hooks）保持启用并已通过全量 lint。
    files: ["**/*.{ts,tsx}"],
    rules: {
      // 命中 9 处：ModulePage/WorkbenchPage 的路由参数变化时重置本地状态、
      // RelationSelect 的目录缺 Action 时置错、DemoItemInsight 的 effect 内取数。
      // 都是合法模式而非缺陷；按新规则重写需要逐组件做「派生状态 / 迁到 TanStack
      // Query」的行为重构，需单独一轮并跑通浏览器门禁后再启用。
      "react-hooks/set-state-in-effect": "off",
      // 命中 3 处（ModulePage:135、WorkbenchPage:168、BusinessPage:79）均为误报：
      // features/registry.ts 的 resolveCustomView 从 Object.freeze 的模块级注册表返回
      // 同一个 LazyExoticComponent 引用，并未在渲染期创建组件。
      "react-hooks/static-components": "off",
      // 命中 1 处（shell/App.tsx:23）：identityResetRef 是标准 latest-ref 模式，
      // ref 只在后续回调里读取，不在渲染期解引用。
      "react-hooks/refs": "off",
      // 命中 1 处（DataGrid 的 useReactTable）：规则自身说明 React Compiler 会跳过
      // 该组件的 memo 化，属 TanStack Table 的上游 API 形态，无可操作项。
      "react-hooks/incompatible-library": "off",
    },
  },
);
