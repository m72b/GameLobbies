// Freezes Object.prototype before any of Game Lobby's own scripts run, so code in this window
// cannot add to or change it (prototype pollution). Loaded first by index.html. This replaces
// Tauri's app-wide `freezePrototype`, which also froze it in the store sign-in windows, where it
// can break the store's own sign-in page (tauri.conf.json sets it to false).
Object.freeze(Object.prototype)
