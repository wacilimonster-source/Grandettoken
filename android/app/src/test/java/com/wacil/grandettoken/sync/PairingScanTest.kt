package com.wacil.grandettoken.sync

import com.wacil.grandettoken.sync.sync.SyncCode
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertIs

class PairingScanTest {
    private val code = SyncCode(
        relayId = ByteArray(16) { it.toByte() },
        key = ByteArray(32) { (it + 16).toByte() },
        keyEpoch = 0,
    ).encode()

    @Test
    fun accepts_trimmed_valid_code() {
        assertEquals(ScanOutcome.Accepted(code), interpretPairingScan("  $code\n"))
    }

    @Test
    fun rejects_cancel_and_foreign_qr() {
        assertIs<ScanOutcome.Rejected>(interpretPairingScan(null))
        assertIs<ScanOutcome.Rejected>(interpretPairingScan("  "))
        assertIs<ScanOutcome.Rejected>(interpretPairingScan("https://example.com"))
    }

    @Test
    fun rejects_truncated_or_corrupt_code() {
        assertIs<ScanOutcome.Rejected>(interpretPairingScan(code.dropLast(2)))
        val i = 10
        val replacement = if (code[i] == 'A') 'B' else 'A'
        assertIs<ScanOutcome.Rejected>(
            interpretPairingScan(code.substring(0, i) + replacement + code.substring(i + 1))
        )
    }
}
