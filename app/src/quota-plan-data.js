(() => {
  const { HOUR, WEEK, EPS } = window.QuotaPlan;
  const confidenceAge = 7 * 24 * HOUR;
  const maxSampleGap = 0.5 * HOUR;

  function kind(c) {
    if (c.id === "opencode-go") return "go";
    if (c.id === "codex") return "codex";
    if (/^antigravity:[a-f0-9]{24}$/.test(c.id)) return "gemini";
    return null;
  }

  function scope(c) {
    return kind(c) === "gemini" ? c.id : /^[a-f0-9]{24}$/.test(c.quotaPlanScope || "")
      ? `${c.id}:${c.quotaPlanScope}` : null;
  }

  function windowOf(w) {
    if (!w || typeof w.remainPercent !== "number" || !Number.isFinite(w.remainPercent)) return null;
    const reset = Date.parse(w.resetsAt);
    return { key: w.key, remaining: w.status === "rate-limited" ? 0 : Math.max(0, Math.min(w.remainPercent, 100)),
      reset: Number.isFinite(reset) ? reset : null };
  }

  function limits(c, settings) {
    const windows = c.windows || [];
    const byKey = key => windowOf(windows.find(w => w.key === key));
    const channel = kind(c);
    if (channel === "go") {
      return { main: byKey("monthly"), week: byKey("weekly"), short: byKey("rolling"), needsShort: true, needsWeek: true, mainPeriod: 0 };
    }
    if (channel === "gemini") {
      const short = byKey("gemini-5h");
      return { main: byKey("gemini-weekly"), short, needsShort: !!short || !settings.weeklyOnly, mainPeriod: WEEK };
    }
    const main = byKey("weekly") || byKey("monthly") || byKey("daily");
    const tier = c.extra?.find(([label]) => label === "套餐")?.[1]?.toLowerCase() || "";
    const short = byKey("5h");
    const needsShort = !!short || (main?.key === "weekly" && !/^pro(?:$|[ _-]?(100|200|500)$)/.test(tier));
    const mainPeriod = main?.key === "weekly" ? WEEK : (main?.key === "monthly" ? 30 : 1) * 24 * HOUR;
    const unsupported = windows.some(w => !["weekly", "monthly", "daily", "5h"].includes(w.key))
      || windows.filter(w => ["weekly", "monthly", "daily"].includes(w.key)).length > 1;
    return { main, short, needsShort, mainPeriod, unsupported };
  }

  function deadline(c, win, settings, profile) {
    if (kind(c) === "go") return win.main?.reset || null;
    if (kind(c) === "gemini") return Number(settings.end) || null;
    return profile?.scope === c.quotaPlanScope && profile.subEndAt > 0 ? profile.subEndAt * 1000 : null;
  }

  function fresh(c, now, maxAge) {
    const at = c.updatedAt * 1000;
    return c.valid && !c.stale && at > 0 && at <= now && now - at <= maxAge;
  }

  function sameWindow(a, b, start, end) {
    return a && b && a.key === b.key && a.reset && a.reset === b.reset && a.reset > end && start < end;
  }

  function sample(previous, win, at, now, hours) {
    if (!win.main || at <= (previous?.last?.at || 0)) return previous;
    const last = { at, main: win.main, short: win.short };
    const history = (previous?.history || []).filter(s => s.at > now - confidenceAge);
    const prior = previous?.last;
    if (prior && at - prior.at >= 60_000 && at - prior.at <= maxSampleGap
        && sameWindow(prior.main, last.main, prior.at, at)) {
      // Only include bounded intervals inside the user's working hours. Polling cannot measure foreground activity.
      const date = new Date(prior.at);
      const [startHour, startMinute] = hours.start.split(":").map(Number);
      const [endHour, endMinute] = hours.end.split(":").map(Number);
      const start = new Date(date).setHours(startHour, startMinute, 0, 0);
      const end = new Date(date).setHours(endHour, endMinute, 0, 0);
      const used = prior.main.remaining - last.main.remaining;
      const paired = sameWindow(prior.short, last.short, prior.at, at);
      const short = paired && prior.short.remaining - last.short.remaining;
      if (prior.at >= start && at <= end && used > EPS && used * HOUR / (at - prior.at) <= 1000) {
        history.push({ at, hours: (at - prior.at) / HOUR, used, short: short > EPS ? short : null });
      }
    }
    return { last, history: history.slice(-32) };
  }

  function pace(history, now) {
    const samples = (history || []).filter(s => s.at > now - confidenceAge);
    let used = 0;
    let hours = 0;
    let paired = 0;
    let short = 0;
    for (const s of samples) {
      used += s.used;
      hours += s.hours;
      if (s.short) {
        paired += s.used;
        short += s.short;
      }
    }
    return { rate: samples.length >= 2 && used >= 1 ? used / hours : null, shortCapacity: short >= 2 && paired >= 0.2 ? 100 * paired / short : null };
  }

  function prepare(c, settings, history, profile, now, maxAge) {
    const win = limits(c, settings);
    const end = deadline(c, win, settings, profile);
    if (!end) return { reason: kind(c) === "go" ? "缺少本期月窗的结束时间" : "请确认订阅到期时间" };
    if (end <= now) return { end, reason: "套餐截止时间已过" };
    if (!fresh(c, now, maxAge)) return { end, reason: "配额待刷新，更新后重排" };
    if (win.unsupported || !win.main?.reset || win.needsWeek && !win.week?.reset || win.needsShort && !win.short) {
      return { end, reason: "适用的额度窗口或重置时间尚未确认" };
    }
    if ([win.main, win.week, win.short].some(w => w?.reset && w.reset <= now)
        || win.short && !win.short.reset && win.short.remaining < 100
        || kind(c) === "codex" && c.limited && [win.main, win.short].every(w => !w || w.remaining > EPS)) {
      return { end, reason: "当前窗口状态待刷新，更新后重排" };
    }
    if (end > now + 370 * 24 * HOUR) return { end, reason: "截止时间超过一年，临近后开始排程" };
    if (win.main.remaining <= EPS && win.main.reset >= end) return { end, exhausted: true, reason: "本期额度已用完" };
    const automatic = pace(history, now);
    const rate = settings.rate || automatic.rate;
    const shortCapacity = win.short ? (kind(c) === "go" ? 20 : settings.shortCapacity || automatic.shortCapacity) : null;
    if (!rate || win.short && !shortCapacity) {
      return { end, reason: !rate ? "用量样本不足，可填写每在线小时用量" : "短窗与主窗的额度比例待确认，可手填试算" };
    }
    return { ...win, end, rate, shortCapacity, mainAt: win.mainPeriod ? win.main.reset : Infinity,
      unit: win.main.key === "monthly" ? "月额度" : win.main.key === "daily" ? "日额度" : "周额度" };
  }

  window.QuotaPlanData = { kind, scope, limits, fresh, sample, prepare };
})();
