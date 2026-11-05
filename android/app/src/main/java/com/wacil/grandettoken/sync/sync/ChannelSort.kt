package com.wacil.grandettoken.sync.sync

enum class ChannelSort(val label: String) {
    Urgency("状态优先"),
    Name("名称"),
    Balance("余额高到低"),
    DayUsage("今日消耗高到低"),
    Custom("自定义"),
}

/** 排序只影响本机展示；不同类型和币种不会放在同一数轴上比较。 */
object ChannelOrdering {
    fun needsAttention(channel: ChannelSnapshot): Boolean =
        channel.stale || channel.errorCode != null ||
            (channel.kind == ChannelKind.Quota &&
                Decimal.parseMicros(channel.percent)?.let { it <= 20_000_000L } == true)

    fun sort(channels: List<ChannelSnapshot>, mode: ChannelSort, customOrder: List<String>): List<ChannelSnapshot> {
        val rank = customOrder.withIndex().associate { it.value to it.index }
        val byName = compareBy<ChannelSnapshot>({ it.name.lowercase() }, { it.platformId })
        return when (mode) {
            ChannelSort.Name -> channels.sortedWith(byName)
            ChannelSort.Custom -> channels.sortedWith(compareBy<ChannelSnapshot> { rank[it.platformId] ?: Int.MAX_VALUE }.then(byName))
            ChannelSort.Balance, ChannelSort.DayUsage -> channels.sortedWith(
                compareBy<ChannelSnapshot>({ it.kind.ordinal }, { it.unit.uppercase() })
                    .thenComparator { a, b -> compareNullableDescending(value(a, mode), value(b, mode)) }
                    .then(byName)
            )
            ChannelSort.Urgency -> channels.sortedWith(
                compareBy<ChannelSnapshot> { urgencyGroup(it) }
                    .thenComparator { a, b ->
                        when (urgencyGroup(a)) {
                            1 -> compareNullableAscending(ratio(a), ratio(b))
                            2, 3 -> compareValues(a.unit.uppercase(), b.unit.uppercase()).takeIf { it != 0 }
                                ?: compareNullableAscending(Decimal.parseMicros(a.remaining)?.toDouble(), Decimal.parseMicros(b.remaining)?.toDouble())
                            else -> 0
                        }
                    }.then(byName)
            )
        }
    }

    private fun urgencyGroup(channel: ChannelSnapshot): Int = when {
        channel.stale || channel.errorCode != null -> 0
        ratio(channel) != null -> 1
        channel.kind == ChannelKind.Money && Decimal.parseMicros(channel.remaining) != null -> 2
        channel.kind == ChannelKind.Points && Decimal.parseMicros(channel.remaining) != null -> 3
        else -> 4
    }

    private fun ratio(channel: ChannelSnapshot): Double? {
        if (channel.kind == ChannelKind.Quota) return Decimal.parseMicros(channel.percent)?.toDouble()
        if (channel.kind != ChannelKind.Money) return null
        val remaining = Decimal.parseMicros(channel.remaining) ?: return null
        val total = Decimal.parseMicros(channel.total) ?: return null
        return if (total > 0) remaining.toDouble() / total.toDouble() * 100_000_000.0 else null
    }

    private fun value(channel: ChannelSnapshot, mode: ChannelSort): Double? = when (mode) {
        ChannelSort.DayUsage -> Decimal.parseMicros(channel.day)?.toDouble()
        else -> Decimal.parseMicros(if (channel.kind == ChannelKind.Quota) channel.percent else channel.remaining)?.toDouble()
    }

    private fun compareNullableAscending(a: Double?, b: Double?): Int = when {
        a == null && b == null -> 0
        a == null -> 1
        b == null -> -1
        else -> a.compareTo(b)
    }

    private fun compareNullableDescending(a: Double?, b: Double?): Int = when {
        a == null && b == null -> 0
        a == null -> 1
        b == null -> -1
        else -> b.compareTo(a)
    }
}
