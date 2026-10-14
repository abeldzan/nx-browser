const banner = document.createElement("div");
banner.textContent = "Hello from NxBrowser";
banner.style.cssText = "position:fixed;top:0;left:0;right:0;z-index:2147483647;background:#0a84ff;color:white;font:13px -apple-system,sans-serif;padding:4px 8px";
const mount = () => {
  if (!document.documentElement || document.getElementById("nx-hello")) return;
  banner.id = "nx-hello";
  document.documentElement.appendChild(banner);
};
if (document.readyState === "loading") {
  document.addEventListener("DOMContentLoaded", mount);
} else {
  mount();
}
browser.storage.local.get("visits").then((value) => {
  const visits = Number(value || 0) + 1;
  browser.storage.local.set({ visits });
  banner.textContent = "Hello from NxBrowser · visits " + visits;
});
browser.runtime.sendMessage({ action: "hello" });
