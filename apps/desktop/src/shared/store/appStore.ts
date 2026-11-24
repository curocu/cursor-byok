import { useSyncExternalStore } from "react";
import { api, type CursorHarnessStatus, type LlmCall, type Model, type ModelInput, type Overview, type PortSettings, type TokenPricingSettings } from "../api";
import { applyTheme, isThemeId, type ThemeId } from "../theme/theme";

export const DEFAULT_TOKEN_PRICING: TokenPricingSettings = {
  input_per_million: 5.0,
  output_per_million: 25.0,
  cache_read_per_million: 0.5,
  cache_write_per_million: 6.25,
};

export type AppSnapshot = {
  models: Model[];
  calls: LlmCall[];
  overview: Overview;
  detailed: boolean;
  ports: PortSettings;
  pricing: TokenPricingSettings;
  busy: boolean;
  error: string | null;
  theme: ThemeId;
  themeReady: boolean;
  themeBusy: boolean;
  themeError: string | null;
  cursorHarness: CursorHarnessStatus | null;
  cursorBusy: boolean;
};

let snapshot: AppSnapshot = {
  models: [],
  calls: [],
  overview: {
    metrics: {
      llm_calls: 0,
      successful_calls: 0,
      failed_calls: 0,
      token_usage: 0,
      prompt_tokens: 0,
      input_tokens: 0,
      cache_read_tokens: 0,
      cache_write_tokens: 0,
      output_tokens: 0,
    },
    token_usage_granularity: "day",
    token_usage_series: [],
  },
  detailed: false,
  ports: { proxy_port: 0, service_port: 0 },
  pricing: DEFAULT_TOKEN_PRICING,
  busy: false,
  error: null,
  theme: "default-dark",
  themeReady: false,
  themeBusy: false,
  themeError: null,
  cursorHarness: null,
  cursorBusy: false,
};

const listeners = new Set<() => void>();

function update(patch: Partial<AppSnapshot>) {
  snapshot = { ...snapshot, ...patch };
  listeners.forEach((listener) => listener());
}

async function perform(task: () => Promise<void>) {
  update({ error: null });
  try {
    await task();
  } catch (cause) {
    update({ error: cause instanceof Error ? cause.message : String(cause) });
  }
}

export const appStore = {
  subscribe(listener: () => void) {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  getSnapshot: () => snapshot,

  async refresh() {
    update({ busy: true, error: null });
    try {
      const [models, calls, overview, settings, ports, pricing, cursorHarness] = await Promise.all([
        api.models(),
        api.calls(),
        api.overview(),
        api.observability(),
        api.ports(),
        api.pricingSettings(),
        api.cursorHarness(),
      ]);
      update({ models, calls, overview, detailed: settings.detailed, ports, pricing, cursorHarness });
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
    } finally {
      update({ busy: false });
    }
  },

  async deleteModel(modelHash: string) {
    await perform(async () => {
      await api.deleteModel(modelHash);
      await appStore.refresh();
    });
  },

  async initializeCursorCa() {
    update({ cursorBusy: true, error: null });
    try {
      const status = await api.initializeCursorCa();
      update({ cursorHarness: status });
      return status;
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
      return null;
    } finally { update({ cursorBusy: false }); }
  },
  async setCursorEnabled(enabled: boolean) {
    update({ cursorBusy: true, error: null });
    try { update({ cursorHarness: await api.setCursorEnabled(enabled) }); }
    catch (cause) { update({ error: cause instanceof Error ? cause.message : String(cause) }); }
    finally { update({ cursorBusy: false }); }
  },
  async createModels(models: ModelInput[]) {
    update({ cursorBusy: true, error: null });
    try {
      const created = await api.createModels(models);
      await appStore.refresh();
      return created;
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
      return null;
    } finally { update({ cursorBusy: false }); }
  },
  async importV0049Models() {
    update({ cursorBusy: true, error: null });
    try {
      const result = await api.importV0049Models();
      await appStore.refresh();
      return result;
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
      return null;
    } finally { update({ cursorBusy: false }); }
  },
  async updateCursorModel(hash: string, model: ModelInput) {
    update({ cursorBusy: true, error: null });
    try {
      const updated = await api.updateModel(hash, model);
      await appStore.refresh();
      return updated;
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
      return null;
    } finally { update({ cursorBusy: false }); }
  },
  async reorderCursorModels(modelHashes: string[]) {
    const previous = snapshot.models;
    const byHash = new Map(previous.map((model) => [model.model_hash, model]));
    if (modelHashes.length !== previous.length || new Set(modelHashes).size !== previous.length) {
      update({ error: t("模型配置已发生变化，请刷新后重试") });
      return false;
    }
    const reordered: Model[] = [];
    for (const [index, hash] of modelHashes.entries()) {
      const model = byHash.get(hash);
      if (!model) {
        update({ error: t("模型配置已发生变化，请刷新后重试") });
        return false;
      }
      reordered.push({ ...model, sort_order: index + 1 });
    }
    update({ models: reordered, cursorBusy: true, error: null });
    try {
      update({ models: await api.reorderModels(modelHashes) });
      return true;
    } catch (cause) {
      update({
        models: previous,
        error: cause instanceof Error ? cause.message : String(cause),
      });
      return false;
    } finally {
      update({ cursorBusy: false });
    }
  },

  async refreshCalls() {
    try {
      update({ calls: await api.calls() });
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
    }
  },

  async openCallDetails(callId: string) {
    await perform(() => api.openCallDetails(callId));
  },
  async updateDetailed(detailed: boolean) {
    await perform(async () => update(await api.setObservability(detailed)));
  },
  async updatePorts(ports: PortSettings) {
    try {
      update({ error: null });
      update({ ports: await api.setPorts(ports) });
      return true;
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
      return false;
    }
  },
  async updatePricingSettings(pricing: TokenPricingSettings) {
    try {
      update({ error: null });
      update({ pricing: await api.setPricingSettings(pricing) });
      return true;
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
      return false;
    }
  },
  async loadTheme() {
    if (snapshot.themeBusy) return;
    update({ themeBusy: true, themeError: null });
    try {
      const theme = await api.theme();
      if (!isThemeId(theme)) throw new Error(t("主题设置无效"));
      applyTheme(theme);
      update({ theme, themeReady: true });
    } catch (cause) {
      update({ themeError: cause instanceof Error ? cause.message : String(cause) });
    } finally {
      update({ themeBusy: false });
    }
  },
  async selectTheme(theme: ThemeId) {
    if (!snapshot.themeReady || snapshot.themeBusy || theme === snapshot.theme) return;
    update({ themeBusy: true, themeError: null });
    try {
      const saved = await api.setTheme(theme);
      if (!isThemeId(saved)) throw new Error(t("主题设置无效"));
      applyTheme(saved);
      update({ theme: saved });
    } catch (cause) {
      update({ themeError: cause instanceof Error ? cause.message : String(cause) });
    } finally {
      update({ themeBusy: false });
    }
  },
};

export function useAppStore() {
  return useSyncExternalStore(appStore.subscribe, appStore.getSnapshot);
}
