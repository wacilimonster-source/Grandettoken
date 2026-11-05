package com.wacil.grandettoken.sync.sync

import org.junit.Assert.assertEquals
import org.junit.Test

class DecimalDisplayTest {
    @Test
    fun `display distinguishes missing values from zero and follows desktop formatting`() {
        assertEquals("——", Decimal.display(null, "CNY"))
        assertEquals("¥0.00", Decimal.display("0", "CNY"))
        assertEquals("¥1,234.50", Decimal.display("1234.5", "CNY"))
        assertEquals("USD 1,234.50", Decimal.display("1234.5", "USD"))
        assertEquals("66.0%", Decimal.display("66", "%"))
        assertEquals("1,240 分", Decimal.display("1240", "credits", points = true))
    }
}
