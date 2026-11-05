package com.wacil.grandettoken.sync

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse

class DiagnosticsTest {
    @Test
    fun groups_codes_without_exposing_source_ids() {
        val sourceA = "a3c8df11756073527dc48a2c9aa1df59"
        val sourceB = "f0e1d6b5b1218967c96672eaa5de696c"
        val summary = summarizeFailures(
            listOf(
                "$sourceA:SYNC_DECRYPT_FAILED",
                "$sourceB:SYNC_DECRYPT_FAILED",
                "$sourceA:SYNC_PAYLOAD_INVALID",
                sourceB,
            )
        )
        assertEquals(
            listOf(
                FailureSummary(2, "SYNC_DECRYPT_FAILED"),
                FailureSummary(1, "SYNC_ERROR"),
                FailureSummary(1, "SYNC_PAYLOAD_INVALID"),
            ),
            summary,
        )
        val text = summary.joinToString { "${it.count} ${it.code}" }
        assertFalse(text.contains(sourceA))
        assertFalse(text.contains(sourceB))
    }
}
