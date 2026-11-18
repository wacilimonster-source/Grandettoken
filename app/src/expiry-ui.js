// 首页临期口径：逐笔金额与账户级有效期是两种不同的数据。
(() => {
  function endOfWindow(days, now) {
    const end = new Date(now);
    end.setHours(0, 0, 0, 0);
    end.setDate(end.getDate() + days);
    return end.getTime() / 1000;
  }

  function qoderCached(channel) {
    if (!["qoder", "qoder-cn"].includes(channel.id)) return false;
    return channel.expiryStale === true || channel.stale === true;
  }

  function entriesWithin(channel, days, now = Date.now()) {
    if (channel.stale || channel.expiryStale) return [];
    const end = endOfWindow(days, now);
    return (channel.expiring || []).filter(entry => entry &&
      Number.isFinite(Number(entry.at)) && Number.isFinite(Number(entry.amount)) &&
      Number(entry.amount) > 0 && Number(entry.at) > now / 1000 && Number(entry.at) < end);
  }

  function accountAt(channel) {
    if (!channel || !["qoder", "qoder-cn"].includes(channel.id)) return null;
    const account = channel.accountExpiry;
    const at = account && account.state === "dated" ? Number(account.at) : NaN;
    return Number.isFinite(at) && at > 0 ? at : null;
  }

  function accountWithin(channel, days, now = Date.now()) {
    if (channel.stale) return null;
    const at = accountAt(channel);
    return at !== null && at > now / 1000 && at < endOfWindow(days, now) ? at : null;
  }

  function eligible(channel) {
    return channel && channel.hasKey && !channel.hidden && channel.valid && channel.kind === "points";
  }

  function nearest(channel, days, now = Date.now()) {
    if (!eligible(channel)) return null;
    const dates = entriesWithin(channel, days, now).map(entry => Number(entry.at));
    const account = accountWithin(channel, days, now);
    if (account !== null) dates.push(account);
    return dates.length ? Math.min(...dates) : null;
  }

  function nextEvent(channel, now = Date.now()) {
    const entries = (channel.stale || channel.expiryStale ? [] : channel.expiring || []).filter(entry => entry &&
      Number.isFinite(Number(entry.at)) && Number.isFinite(Number(entry.amount)) &&
      Number(entry.amount) > 0 && Number(entry.at) > now / 1000);
    entries.sort((a, b) => Number(a.at) - Number(b.at));
    const account = accountAt(channel);
    if (account !== null && account > now / 1000 && (!entries.length || account <= Number(entries[0].at))) {
      return { type: "account", at: account };
    }
    return entries.length ? { type: "points", at: Number(entries[0].at), entry: entries[0] } : null;
  }

  function summary(channels, days, now = Date.now()) {
    const visible = channels.filter(eligible);
    const unknown = visible.filter(channel => (channel.expiryComplete !== true || channel.stale || channel.expiryStale) && Number(channel.remaining || 0) > 0);
    const entries = visible.flatMap(channel => entriesWithin(channel, days, now)
      .map(entry => ({ channel, entry, type: "points" })));
    const accounts = visible.flatMap(channel => {
      const at = accountWithin(channel, days, now);
      return at === null ? [] : [{ channel, entry: { at }, type: "account" }];
    });
    entries.sort((a, b) => Number(a.entry.at) - Number(b.entry.at));
    const first = [...entries, ...accounts].sort((a, b) => Number(a.entry.at) - Number(b.entry.at))[0] || null;
    return {
      total: Math.round(entries.reduce((sum, item) => sum + Number(item.entry.amount), 0) * 100) / 100,
      channels: new Set(entries.map(item => item.channel.id)).size,
      entries, accounts, unknown, first,
      hasConfirmed: entries.length > 0 || accounts.length > 0,
    };
  }

  function dayIndex(at, now = Date.now()) {
    const ordinal = date => Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()) / 86400000;
    return ordinal(new Date(at * 1000)) - ordinal(new Date(now));
  }

  function qoderNotice(channel) {
    const line = (channel.extra || []).find(item => item[0] === "逐笔到期明细");
    const detail = line && String(line[1] || "");
    const cached = qoderCached(channel);
    if (channel.stale || channel.valid === false) {
      return { text: "渠道数据未更新 · 逐笔明细可能是旧记录", fresh: false, button: "重新登录" };
    }
    if (channel.expiryComplete === true && !cached) {
      return { text: "已登录网页账号 · 逐笔明细可用", fresh: true, button: "重新登录" };
    }
    return { text: detail && detail !== "接口未提供" ? detail : cached ? "逐笔明细沿用旧记录，当前状态未知" : "逐笔明细暂不可用，可登录网页账号获取",
      fresh: false, button: cached ? "重新登录" : "登录网页版" };
  }

  window.ExpiryUI = { entriesWithin, accountWithin, nearest, nextEvent, summary, dayIndex, qoderNotice };
})();
