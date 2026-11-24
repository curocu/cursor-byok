import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import test from "node:test";
import vm from "node:vm";

const storeSource = stripTypeScriptTypes(await readFile(new URL("./appStore.ts", import.meta.url), "utf8"));
const themeSource = stripTypeScriptTypes(await readFile(new URL("../theme/theme.ts", import.meta.url), "utf8"));

async function createStore(api) {
  const root = { dataset: {}, style: { setProperty(name, value) { this[name] = value; } } };
  const meta = { setAttribute(name, value) { this[name] = value; } };
  const context = vm.createContext({
    document: { documentElement: root, querySelector: () => meta },
    t: (text) => text,
    Error,
    get localStorage() { throw new Error("WebView Local Storage is unavailable"); },
  });
  const theme = new vm.SourceTextModule(themeSource, { context });
  const react = new vm.SyntheticModule(["useSyncExternalStore"], function () {
    this.setExport("useSyncExternalStore", () => {});
  }, { context });
  const apiModule = new vm.SyntheticModule(["api"], function () { this.setExport("api", api); }, { context });
  const store = new vm.SourceTextModule(storeSource, { context });
  await store.link((specifier) => {
    if (specifier === "react") return react;
    if (specifier === "../api") return apiModule;
    if (specifier === "../theme/theme") return theme;
    throw new Error(`Unexpected import: ${specifier}`);
  });
  await store.evaluate();
  return { store: store.namespace.appStore, root, meta };
}

test("theme survives a new frontend with Local Storage completely unavailable", async () => {
  let persisted = "default-dark";
  const api = { theme: async () => persisted, setTheme: async (value) => (persisted = value) };
  const first = await createStore(api);
  await first.store.loadTheme();
  await first.store.selectTheme("default-light");
  assert.equal(first.store.getSnapshot().theme, "default-light");
  assert.equal(first.root.dataset.theme, "default-light");
  assert.equal(first.root.style.colorScheme, "light");
  assert.equal(first.meta.content, "#fcfcfc");
  const reopened = await createStore(api);
  await reopened.store.loadTheme();
  assert.equal(reopened.store.getSnapshot().theme, "default-light");
  await reopened.store.selectTheme("default-dark");
  assert.equal(persisted, "default-dark");
});

test("selection applies only after persistence succeeds and concurrent clicks cannot race", async () => {
  let acknowledge;
  let writes = 0;
  const { store, root } = await createStore({
    theme: async () => "default-dark",
    setTheme: () => { writes++; return new Promise((resolve) => { acknowledge = resolve; }); },
  });
  await store.loadTheme();
  const save = store.selectTheme("default-light");
  assert.equal(store.getSnapshot().themeBusy, true);
  assert.equal(root.dataset.theme, "default-dark");
  await store.selectTheme("default-light");
  assert.equal(writes, 1);
  acknowledge("default-light");
  await save;
  assert.equal(root.dataset.theme, "default-light");
  assert.equal(store.getSnapshot().themeBusy, false);
});

test("failed writes preserve the selected theme and expose the error", async () => {
  const { store, root } = await createStore({
    theme: async () => "default-light",
    setTheme: async () => { throw new Error("database is read-only"); },
  });
  await store.loadTheme();
  await store.selectTheme("default-dark");
  assert.equal(root.dataset.theme, "default-light");
  assert.equal(store.getSnapshot().theme, "default-light");
  assert.equal(store.getSnapshot().themeError, "database is read-only");
  assert.equal(store.getSnapshot().themeBusy, false);
});

test("failed initial read cannot overwrite settings and can be retried", async () => {
  let available = false;
  let writes = 0;
  const { store } = await createStore({
    theme: async () => { if (!available) throw new Error("offline"); return "default-light"; },
    setTheme: async () => { writes++; return "default-dark"; },
  });
  await store.loadTheme();
  assert.equal(store.getSnapshot().themeReady, false);
  assert.equal(store.getSnapshot().themeError, "offline");
  await store.selectTheme("default-dark");
  assert.equal(writes, 0);
  available = true;
  await store.loadTheme();
  assert.equal(store.getSnapshot().themeReady, true);
  assert.equal(store.getSnapshot().theme, "default-light");
  assert.equal(store.getSnapshot().themeError, null);
});

test("invalid persisted theme is reported, not silently treated as a dark preference", async () => {
  const { store } = await createStore({ theme: async () => "invalid" });
  await store.loadTheme();
  assert.equal(store.getSnapshot().themeReady, false);
  assert.equal(store.getSnapshot().themeError, "主题设置无效");
});
