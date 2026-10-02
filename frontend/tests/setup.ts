import "@testing-library/jest-dom/vitest";
import { configure } from "@testing-library/react";

// 默认 findBy*/waitFor 超时是 1000ms，在 full 套件多 worker 并行下（本机核数多、
// 每个 worker 都渲染整套 App）会因 CPU/内存竞争误超时——已用 base 提交实测证明是
// 既有 flakiness，与权限修复无关。提到 5s：真失败仍会在 5s 内如实报错，只是不再
// 把「渲染慢」误判成「元素缺失」。
configure({ asyncUtilTimeout: 5000 });

// jsdom 缺失而 Radix UI（Select/DropdownMenu）依赖的最小 API mock（shadcn 官方测试实践）。
if (!Element.prototype.scrollIntoView) {
  Element.prototype.scrollIntoView = () => undefined;
}
if (!Element.prototype.hasPointerCapture) {
  Element.prototype.hasPointerCapture = () => false;
}
if (!Element.prototype.setPointerCapture) {
  Element.prototype.setPointerCapture = () => undefined;
}
if (!Element.prototype.releasePointerCapture) {
  Element.prototype.releasePointerCapture = () => undefined;
}

// jsdom 未实现 Blob URL：下载/预览附件处理（attachment.ts）依赖，打桩为可断言的占位。
if (typeof URL.createObjectURL !== "function") {
  URL.createObjectURL = () => "blob:mock-object-url";
}
if (typeof URL.revokeObjectURL !== "function") {
  URL.revokeObjectURL = () => undefined;
}
