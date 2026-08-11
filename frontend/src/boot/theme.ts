import { defineBoot } from "#q-app/wrappers";
import { Dark } from "quasar";

// 深浅色主题：启动时恢复 localStorage 中的偏好，无保存值时跟随系统
export default defineBoot(() => {
  const saved = localStorage.getItem("ys-theme");
  Dark.set(saved === "dark" ? true : saved === "light" ? false : "auto");
});
