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
  const listeners = new Map();

  function getState() {
    return { ...state };
  }

  function setState(patch) {
    const changed = [];
    for (const [slice, value] of Object.entries(patch)) {
      if (!Object.hasOwn(state, slice)) throw new TypeError("未知的状态字段 " + slice);
      if (Object.is(state[slice], value)) continue;
      state[slice] = value;
      changed.push(slice);
    }
    for (const slice of changed) {
      for (const listener of [...(listeners.get(slice) || [])]) listener(state[slice]);
    }
  }

  function subscribe(slice, listener) {
    if (!Object.hasOwn(state, slice)) throw new TypeError("未知的状态字段 " + slice);
    if (!listeners.has(slice)) listeners.set(slice, new Set());
    const group = listeners.get(slice);
    group.add(listener);
    return () => group.delete(listener);
  }

  function setTabDirty(tab, dirty) {
    if (state.dirtyTabs.has(tab) === dirty) return false;
    const dirtyTabs = new Set(state.dirtyTabs);
    if (dirty) dirtyTabs.add(tab);
    else dirtyTabs.delete(tab);
    setState({ dirtyTabs });
    return true;
  }

  // Existing classic scripts share these accessors while callers migrate to GT_STORE.
  for (const [name, slice] of Object.entries({
    CHANNELS: "channels", HV: "homeView", CFG: "config", WINDOW_VISIBLE: "windowVisible",
    currentTab: "activeTab", DETAIL_ID: "detailId",
  })) {
    Object.defineProperty(globalThis, name, {
      get: () => state[slice],
      set: (value) => setState({ [slice]: value }),
    });
  }
  window.GT_STORE = { getState, setState, subscribe, setTabDirty };
})();
