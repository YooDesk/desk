
## 2026-11-12 Android and account devices

Migrated upstream `c0975f27b05dc33dd303742cea519f390552fb4c` from the read-only `E:/YooDesk` checkout.
Android capture, input, consent deadlines and frame ownership live in this plugin. The Android build disables the desktop engine feature, exports the existing plugin ABI plus the Android capture C bridge, and builds pinned RustDesk scrap in an ignored checkout. Browser connection deadlines allow local capture consent.

## 2026-11-20 Viewer carrier resume and binary Desk files

Read-only migration source: `E:/YooDesk`, commit `bdd69273495ec7cfe9b6b2750416959c7777ec39`.

Migrated binary streaming upload/download, file block engine RPC, five-session default and plugin-owned browser assets. The browser enables streaming explicitly. Host authentication callbacks and the versioned plugin ABI remain authoritative.
