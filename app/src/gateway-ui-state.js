/* 网关视图的读取代际和编辑版本；后端快照不能拥有正在编辑的字段。 */
(() => {
  window.createGatewayUiState = function createGatewayUiState() {
    const generations = { state: 0, diag: 0 };
    let writes = 0;
    let diagWrites = 0;
    let revision = 0;
    let fields = new Map();
    let syncRevision = -1n;
    let syncState = null;
    const valueOf = (el) => el.type === "checkbox" ? el.checked : el.value;
    const keyOf = (el) => {
      if (el.id) return el.id;
      const card = el.closest("[data-gw-card]")?.dataset.gwCard;
      const attr = [...el.attributes].find((a) => a.name.startsWith("data-gw-"));
      return attr ? `${card || ""}/${attr.name}/${attr.value}/${attr.name === "data-gw-model" ? el.value : ""}` : null;
    };
    function controls(box) {
      return [...(box?.querySelectorAll("input, textarea, select") || [])].map((el) => [keyOf(el), el]).filter(([key]) => key);
    }
    function observe(box) {
      for (const [key, el] of controls(box)) {
        const field = fields.get(key);
        const value = valueOf(el);
        if (field && field.value !== value) {
          field.value = value;
          field.revision = ++revision;
          field.dirty = value !== field.baseline;
        }
      }
    }
    function invalidateReads() {
      generations.state++;
      generations.diag++;
    }
    return {
      read(kind) { return ++generations[kind]; },
      current(kind, ticket) {
        if (kind === "diag" && diagWrites) return false;
        return ticket === generations[kind] && writes === 0;
      },
      beginWrite() { writes++; invalidateReads(); },
      endWrite() { writes--; invalidateReads(); },
      beginDiagWrite() { diagWrites++; generations.diag++; },
      endDiagWrite() { diagWrites--; generations.diag++; },
      observe,
      hasDirty(scope) { return controls(scope).some(([key]) => fields.get(key)?.dirty); },
      bind(box) {
        fields = new Map(controls(box).map(([key, el]) => {
          const value = valueOf(el);
          return [key, { value, baseline: value, revision: ++revision, dirty: false }];
        }));
      },
      snapshot(box) {
        observe(box);
        const active = document.activeElement;
        return {
          dirty: new Map([...fields].filter(([, field]) => field.dirty).map(([key, field]) => [key, { ...field }])),
          focus: box.contains(active) ? keyOf(active) : null,
          start: active?.selectionStart, end: active?.selectionEnd, direction: active?.selectionDirection,
        };
      },
      restore(box, snap) {
        if (!snap) return;
        for (const [key, el] of controls(box)) {
          const dirty = snap.dirty.get(key);
          if (dirty) {
            if (el.type === "checkbox") el.checked = dirty.value;
            else el.value = dirty.value;
            const canonical = fields.get(key);
            fields.set(key, { ...dirty, baseline: canonical.baseline });
          }
          if (key === snap.focus) {
            el.focus({ preventScroll: true });
            if (typeof snap.start === "number" && typeof el.setSelectionRange === "function") {
              el.setSelectionRange(snap.start, snap.end, snap.direction || "none");
            }
          }
        }
      },
      submitted(box, scope) {
        observe(box);
        return new Map(controls(scope).map(([key]) => [key, { ...fields.get(key) }]));
      },
      commit(box, submitted) {
        observe(box);
        for (const [key, field] of submitted || []) {
          const current = fields.get(key);
          if (current && current.revision === field.revision) {
            current.dirty = false;
            current.baseline = current.value;
          }
        }
      },
      sync(next) {
        if (!next || typeof next.pending !== "boolean" || !(next.error == null || typeof next.error === "string")) return false;
        let revision;
        try { revision = BigInt(next.revision); } catch { return false; }
        if (revision < syncRevision) return false;
        syncRevision = revision;
        syncState = { pending: next.pending, error: next.error ?? null, revision: next.revision };
        return true;
      },
      syncState() { return syncState; },
    };
  };
})();
