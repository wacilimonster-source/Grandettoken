package com.wacil.grandettoken.sync.sync

/**
 * 跨来源合并：按 platformId 取最新整条记录，不跨来源拼接、不求和。
 * 与 contract/README.md 第 6 节一致。
 */
object SyncMerge {
    private val platformPattern = Regex("^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$")

    fun validate(snap: SyncSnapshot): Boolean {
        if (snap.v != 1) return false
        if (!SyncCrypto.validSource(snap.source)) return false
        if (snap.seq <= 0 || snap.at <= 0) return false
        if (snap.app.length > 40) return false
        if (snap.channels.size > 64) return false
        for (ch in snap.channels) {
            if (!platformPattern.matches(ch.platformId)) return false
            if (ch.id.isEmpty() || ch.id.length > 64) return false
            if (ch.name.isBlank() || ch.name.length > 40) return false
            if (ch.unit.isBlank() || ch.unit.length > 16 || ch.unit.any { it == '|' }) return false
            if (ch.updatedAt <= 0) return false
            if (ch.windows.size > 16 || ch.expiring.size > 64 || ch.history.size > 7) return false
            if (!Decimal.valid(ch.remaining) && ch.remaining != null) return false
            if (!Decimal.valid(ch.total) && ch.total != null) return false
            if (!Decimal.valid(ch.percent) && ch.percent != null) return false
            if (!Decimal.valid(ch.day) && ch.day != null) return false
            if (!Decimal.valid(ch.week) && ch.week != null) return false
            if (!Decimal.valid(ch.month) && ch.month != null) return false
            for (w in ch.windows) {
                if (w.label.length > 40) return false
                if (w.remainPercent != null && !Decimal.valid(w.remainPercent)) return false
            }
            for (e in ch.expiring) {
                if (!Decimal.valid(e.amount) || e.at <= 0) return false
            }
            for (h in ch.history) {
                if (h.periodEnd <= h.periodStart || h.revisedAt < 0) return false
                if (h.balance != null && !Decimal.valid(h.balance)) return false
                if (h.spent != null && !Decimal.valid(h.spent)) return false
            }
        }
        snap.checkin?.let { ck ->
            if (ck.items.size > 64) return false
            if (ck.done > ck.total || ck.total > ck.items.size) return false
            if (ck.items.any { it.itemId.isEmpty() || it.observedAt <= 0 }) return false
        }
        return true
    }

    fun merge(
        snapshots: List<SyncSnapshot>,
        failedSources: List<String> = emptyList(),
    ): MergedSnapshot {
        val failed = failedSources.toMutableList()
        val bySource = LinkedHashMap<String, SyncSnapshot>()
        for (snap in snapshots) {
            if (!validate(snap)) {
                failed.add("${snap.source}:${SyncCodes.PAYLOAD_INVALID}")
                continue
            }
            val old = bySource[snap.source]
            if (old == null) {
                bySource[snap.source] = snap
            } else if (snap.seq > old.seq) {
                bySource[snap.source] = snap
            } else if (snap.seq == old.seq && snap != old) {
                failed.add("${snap.source}:${SyncCodes.CONFLICT}")
            }
        }

        val picked = LinkedHashMap<String, Pair<String, ChannelSnapshot>>()
        for (snap in bySource.values) {
            for (ch in snap.channels) {
                val cur = picked[ch.platformId]
                if (cur == null ||
                    ch.updatedAt > cur.second.updatedAt ||
                    (ch.updatedAt == cur.second.updatedAt && snap.source < cur.first)
                ) {
                    picked[ch.platformId] = snap.source to ch
                }
            }
        }

        val history = LinkedHashMap<HistKey, HistPick>()
        for (snap in bySource.values) {
            for (ch in snap.channels) {
                for (point in ch.history) {
                    val key = HistKey(ch.platformId, point.periodStart, point.periodEnd, point.unit)
                    val cur = history[key]
                    if (cur == null ||
                        point.revisedAt > cur.revisedAt ||
                        (point.revisedAt == cur.revisedAt && snap.source < cur.source)
                    ) {
                        history[key] = HistPick(point.revisedAt, snap.source, point)
                    }
                }
            }
        }

        val channels = picked.entries.sortedBy { it.key }.map { (platformId, pair) ->
            val hist = history.entries
                .filter { it.key.platformId == platformId }
                .map { it.value.point }
                .sortedBy { it.periodStart }
                .takeLast(7)
            pair.second.copy(history = hist)
        }

        val checkin = mergeCheckin(bySource.values)
        val totals = recomputeTotals(channels)
        return MergedSnapshot(
            channels = channels,
            checkin = checkin,
            totals = totals,
            sources = bySource.size,
            failedSources = failed.distinct(),
            generatedAt = bySource.values.maxOfOrNull { it.at } ?: 0,
            staleChannels = channels.count { it.stale },
        )
    }

    private fun mergeCheckin(snapshots: Collection<SyncSnapshot>): CheckinSnapshot? {
        val date = snapshots.mapNotNull { it.checkin?.date }.maxOrNull() ?: return null
        val items = LinkedHashMap<String, Pair<Long, Pair<String, CheckinItem>>>()
        for (snap in snapshots) {
            val ck = snap.checkin ?: continue
            if (ck.date != date) continue
            for (item in ck.items) {
                val cur = items[item.itemId]
                if (cur == null ||
                    item.observedAt > cur.first ||
                    (item.observedAt == cur.first && snap.source < cur.second.first)
                ) {
                    items[item.itemId] = item.observedAt to (snap.source to item)
                }
            }
        }
        if (items.isEmpty()) return null
        val merged = items.values.map { it.second.second }.sortedBy { it.itemId }
        val done = merged.count { it.status == CheckinStatus.Done }
        val pending = merged.count { it.status == CheckinStatus.Pending }
        val unsupported = merged.count { it.status == CheckinStatus.Unsupported }
        val unknown = merged.count { it.status == CheckinStatus.Unknown }
        return CheckinSnapshot(
            date = date,
            timeZone = "Asia/Shanghai",
            items = merged,
            done = done,
            total = done + pending,
            unsupported = unsupported,
            unknown = unknown,
        )
    }

    private fun recomputeTotals(channels: List<ChannelSnapshot>): TotalsSnapshot {
        val money = channels.filter { it.kind == ChannelKind.Money }
        val cny = Decimal.sum(money.filter { it.unit == "CNY" }.map { it.remaining })
        val today = Decimal.sum(money.filter { it.unit == "CNY" }.map { it.day })
        val others = money
            .filter { it.unit != "CNY" }
            .groupBy { it.unit }
            .mapNotNull { (unit, list) ->
                Decimal.sum(list.map { it.remaining })?.let { OtherMoney(unit, it) }
            }
            .sortedBy { it.unit }
        return TotalsSnapshot(cny = cny, others = others, today = today)
    }

    private data class HistKey(
        val platformId: String,
        val periodStart: Long,
        val periodEnd: Long,
        val unit: String,
    )

    private data class HistPick(
        val revisedAt: Long,
        val source: String,
        val point: HistoryPoint,
    )
}
