browser.runtime.onMessage.addListener((message) => {
  browser.storage.local.set({ lastAction: message.action || "hello" });
});
browser.storage.local.set({ booted: true });
