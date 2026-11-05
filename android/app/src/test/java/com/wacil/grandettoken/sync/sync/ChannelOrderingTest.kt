package com.wacil.grandettoken.sync.sync

import org.junit.Assert.assertEquals
import org.junit.Test

class ChannelOrderingTest {
    private fun channel(id: String, kind: ChannelKind = ChannelKind.Money, unit: String = "CNY", remaining: String? = null, day: String? = null, stale: Boolean = false) =
        ChannelSnapshot(platformId = id, id = id, name = id, kind = kind, unit = unit, remaining = remaining, day = day, stale = stale, updatedAt = 1)

    @Test
    fun `custom order preserves missing and newly added channels`() {
        val channels = listOf(channel("b"), channel("c"), channel("a"))
        assertEquals(listOf("b", "a", "c"), ChannelOrdering.sort(channels, ChannelSort.Custom, listOf("b", "old", "a")).map { it.id })
    }

    @Test
    fun `urgency puts stale first and smaller known quota before missing values`() {
        val channels = listOf(
            channel("unknown", ChannelKind.Quota, "%"),
            channel("low", ChannelKind.Quota, "%").copy(percent = "12"),
            channel("high", ChannelKind.Quota, "%").copy(percent = "80"),
            channel("stale", stale = true),
        )
        assertEquals(listOf("stale", "low", "high", "unknown"), ChannelOrdering.sort(channels, ChannelSort.Urgency, emptyList()).map { it.id })
    }

    @Test
    fun `balance sort groups currency and leaves missing values last within the group`() {
        val channels = listOf(channel("missing"), channel("small", remaining = "2"), channel("large", remaining = "10"), channel("usd", unit = "USD", remaining = "100"))
        assertEquals(listOf("large", "small", "missing", "usd"), ChannelOrdering.sort(channels, ChannelSort.Balance, emptyList()).map { it.id })
    }
}
