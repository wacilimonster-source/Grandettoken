(() => {
  "use strict";
  const state = {
    channels: [],
    homeView: { byId: {}, order: [], checkin: null, actionCount: 0, staleCount: 0, errorCount: 0 },
    config: null,
    windowVisible: true,
    activeTab: "index",
    dirtyTabs: new Set(),
    detailId: null,
  };
  function setTabDirty(tab, dirty) {
    const previous = state.dirtyTabs.has(tab);
    if (dirty) state.dirtyTabs.add(tab);
    else state.dirtyTabs.delete(tab);
    return previous;
  }

  // Existing classic scripts share these accessors while callers migrate to GT_STORE.
  for (const [name, slice] of Object.entries({
    CHANNELS: "channels", HV: "homeView", CFG: "config", WINDOW_VISIBLE: "windowVisible",
    currentTab: "activeTab", DETAIL_ID: "detailId",
  })) {
    Object.defineProperty(globalThis, name, {
      get: () => state[slice],
      set: (value) => { state[slice] = value; },
    });
  }
  window.GT_STORE = { setTabDirty };
})();
