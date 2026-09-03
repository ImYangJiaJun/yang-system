import "@testing-library/jest-dom/vitest";

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
