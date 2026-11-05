package com.wacil.grandettoken.sync

import com.wacil.grandettoken.sync.sync.ChannelKind
import com.wacil.grandettoken.sync.sync.ChannelSnapshot
import com.wacil.grandettoken.sync.sync.MergedSnapshot
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class NotificationRulesTest {
    private fun money(id: String, remaining: String?, unit: String = "CNY") = ChannelSnapshot(
        platformId = id,
        id = id,
        name = id,
        kind = ChannelKind.Money,
        unit = unit,
        remaining = remaining,
        updatedAt = 100,
    )

    private fun quota(id: String, percent: String?) = ChannelSnapshot(
        platformId = id,
        id = id,
        name = id,
        kind = ChannelKind.Quota,
        unit = "%",
        percent = percent,
        updatedAt = 100,
    )

    @Test
    fun low_balance_uses_cny_and_percent_thresholds() {
        val merged = MergedSnapshot(
            channels = listOf(
                money("low", "9.99"),
                money("edge", "10.00"),
                money("safe", "10.01"),
                money("usd", "1.00", "USD"),
                quota("quota-low", "19.9"),
                quota("quota-safe", "20.1"),
            )
        )
        assertEquals(
            listOf("low", "edge", "quota-low"),
            NotificationRules.lowBalance(merged).map { it.platformId },
        )
    }

    @Test
    fun stale_includes_old_and_failed_channels() {
        val now = 10_000L
        val merged = MergedSnapshot(
            channels = listOf(
                money("old", "100").copy(updatedAt = now - NotificationRules.STALE_SECONDS),
                money("fresh", "100").copy(updatedAt = now - 60),
                money("failed", "100").copy(errorCode = "SYNC_UPSTREAM", updatedAt = now - 60),
            )
        )
        assertEquals(
            listOf("old", "failed"),
            NotificationRules.stale(merged, now).map { it.platformId },
        )
    }

    @Test
    fun failure_needs_three_consecutive_attempts() {
        assertFalse(NotificationRules.failureReached(2))
        assertTrue(NotificationRules.failureReached(3))
    }
}
