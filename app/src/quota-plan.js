/* Schedule independent Agent pools within the same local working hours. */
(() => {
  const HOUR = 3600000;
  const WEEK = 7 * 24 * HOUR;
  const EPS = 0.05;
  const maxWork = 10 * HOUR;
  const defaultHours = { start: "09:00", end: "19:00" };

  function clockMinutes(value) {
    if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(value || "")) return NaN;
    const [h, m] = value.split(":").map(Number);
    return h * 60 + m;
  }

  function validHours(hours) {
    const span = clockMinutes(hours.end) - clockMinutes(hours.start);
    return span > 0 && span <= 600;
  }

  function daysBetween(now, end, hours) {
    const days = [];
    const date = new Date(now);
    date.setHours(0, 0, 0, 0);
    for (; date.getTime() < end; date.setDate(date.getDate() + 1)) {
      const start = new Date(date).setMinutes(clockMinutes(hours.start));
      const stop = new Date(date).setMinutes(clockMinutes(hours.end));
      days.push({ day: date.getTime(), start: Math.max(now, start), end: Math.min(end, stop, start + maxWork) });
    }
    return days;
  }

  function nextPeriodic(reset, period, t) {
    return reset + (1 + Math.floor((t - reset) / period)) * period;
  }

  function replenish(s, plan, t) {
    if (s.mainAt <= t) {
      s.mainAt = nextPeriodic(s.mainAt, plan.mainPeriod, t);
      s.main = 100;
    }
    if (s.weekAt <= t) {
      s.weekAt = nextPeriodic(s.weekAt, WEEK, t);
      s.week = 50;
    }
    if (s.shortAt !== null && s.shortAt <= t) {
      s.short = plan.shortCapacity;
      s.shortAt = null;
    }
  }

  function workDay(plan, previous, day, skipTail) {
    const s = { ...previous };
    let at = day.start;
    let shortWindows = 0;
    let lastShortAt = null;
    const work = [];
    if (skipTail) at = Math.max(at, s.shortAt);
    while (at < day.end) {
      replenish(s, plan, at);
      const stop = Math.min(day.end, s.mainAt, s.weekAt, s.shortAt ?? Infinity);
      const available = Math.min(s.main, s.week, s.short);
      if (available <= EPS) {
        at = stop;
        continue;
      }
      if (plan.shortCapacity) {
        const shortAt = s.shortAt ?? (at + 5 * HOUR);
        if (lastShortAt !== shortAt && shortWindows >= 2) break;
        if (lastShortAt !== shortAt) shortWindows++;
        lastShortAt = shortAt;
        s.shortAt = shortAt;
      }
      const until = Math.min(stop, s.shortAt ?? Infinity);
      const amount = Math.min(available, (until - at) * plan.rate / HOUR);
      if (amount <= 0) break;
      const end = at + amount * HOUR / plan.rate;
      const due = Math.min(s.mainAt, plan.end);
      work.push({ start: at, end, amount, due, shortAt: s.shortAt });
      s.main -= amount;
      s.week -= amount;
      s.short -= amount;
      at = Math.min(until, end);
    }
    return { state: s, work, used: work.reduce((total, slot) => total + slot.amount, 0) };
  }

  function simulate(plan, days, first, earliest = -Infinity) {
    let state = {
      main: plan.main.remaining,
      mainAt: plan.mainAt,
      week: plan.week ? plan.week.remaining * 0.5 : Infinity,
      weekAt: plan.week ? plan.week.reset : Infinity,
      short: plan.short ? plan.short.remaining * plan.shortCapacity / 100 : Infinity,
      shortAt: plan.short?.reset || null,
    };
    const schedule = [];
    const periods = new Map();
    for (const interval of days.slice(first)) {
      const day = interval.start < earliest ? { ...interval, start: earliest } : interval;
      if (day.start >= day.end) continue;
      const normal = workDay(plan, state, day, false);
      // A short tail can consume one of today's two windows with very little useful time.
      const skip = state.shortAt > day.start && state.shortAt < day.end
        ? workDay(plan, state, day, true) : normal;
      const best = skip.used > normal.used + EPS ? skip : normal;
      state = best.state;
      if (!best.work.length) continue;
      const slots = [];
      for (const work of best.work) {
        periods.set(work.due, (periods.get(work.due) || 0) + work.amount);
        const last = slots[slots.length - 1];
        if (last && last.shortAt === work.shortAt && last.due === work.due && Math.abs(last.end - work.start) < 1) {
          last.end = work.end;
          last.amount += work.amount;
        } else {
          slots.push({ ...work });
        }
      }
      schedule.push({ ...day, slots });
    }
    return { schedule, periods };
  }

  function schedule(plan, now, hours = defaultHours) {
    const days = daysBetween(now, plan.end, hours);
    const full = simulate(plan, days, 0);
    function missingIfSkipped(first, earliest) {
      const remaining = simulate(plan, days, first, earliest).periods;
      return [...full.periods].some(([due, amount]) => amount - (remaining.get(due) || 0) > EPS);
    }
    let low = 0;
    let high = days.length;
    // Every possible schedule after a later day is also available at an earlier start.
    while (low + 1 < high) {
      const middle = Math.floor((low + high) / 2);
      if (missingIfSkipped(middle)) high = middle;
      else low = middle;
    }
    const first = full.schedule[0];
    const firstDay = first ? Math.max(low, days.findIndex(day => day.day === first.day)) : -1;
    let latestStart = null;
    if (firstDay >= 0) {
      let from = days[firstDay].start;
      let to = days[firstDay].end;
      while (to - from > 5 * 60000) {
        const middle = Math.floor((from + to) / 2);
        if (missingIfSkipped(firstDay, middle)) to = middle;
        else from = middle;
      }
      latestStart = from;
    }
    const mainDue = Math.min(plan.mainAt, plan.end);
    let incomplete = plan.main.remaining - (full.periods.get(mainDue) || 0) > EPS;
    const resets = plan.mainPeriod > 0 ? Math.max(0, Math.ceil((plan.end - plan.mainAt) / plan.mainPeriod)) : 0;
    if (resets) {
      const cleared = [...full.periods].filter(([due, used]) => due > mainDue && used >= 100 - EPS).length;
      incomplete ||= cleared < resets;
    }
    return {
      schedule: full.schedule, incomplete,
      latestStart,
      urgent: firstDay === 0 && missingIfSkipped(1),
    };
  }

  window.QuotaPlan = { HOUR, WEEK, EPS, defaultHours, validHours, schedule };
})();
