package com.wacil.grandettoken.sync.sync

import java.math.BigDecimal
import java.math.RoundingMode
import java.text.DecimalFormat
import java.text.DecimalFormatSymbols
import java.util.Locale

/** 十进制口径：可带负号、最多 6 位小数、最多 24 字符；拒绝 NaN/Inf/指数。 */
object Decimal {
    private val pattern = Regex("^-?\\d+(\\.\\d{1,6})?$")

    fun valid(value: String?): Boolean {
        if (value == null) return false
        val s = value.trim()
        return s.isNotEmpty() && s.length <= 24 && pattern.matches(s)
    }

    fun parseMicros(value: String?): Long? {
        if (!valid(value)) return null
        return try {
            BigDecimal(value!!.trim())
                .movePointRight(6)
                .setScale(0, RoundingMode.UNNECESSARY)
                .longValueExact()
        } catch (_: Exception) {
            null
        }
    }

    fun formatMicros(micros: Long): String {
        if (micros == 0L) return "0"
        return BigDecimal.valueOf(micros, 6)
            .stripTrailingZeros()
            .toPlainString()
    }

    fun sum(values: Iterable<String?>): String? {
        var total = 0L
        var any = false
        for (v in values) {
            val micros = parseMicros(v) ?: continue
            total += micros
            any = true
        }
        return if (any) formatMicros(total) else null
    }

    fun fromDouble(value: Double?): String? {
        if (value == null || !value.isFinite() || kotlin.math.abs(value) > 1_000_000_000_000.0) {
            return null
        }
        return BigDecimal.valueOf(value)
            .setScale(6, RoundingMode.HALF_UP)
            .stripTrailingZeros()
            .toPlainString()
    }

    /** UI 展示：金额保留两位；积分不强制小数。 */
    fun display(value: String?, unit: String, points: Boolean = false): String {
        if (value == null) return "——"
        return try {
            val bd = BigDecimal(value)
            val pattern = when {
                unit == "%" -> "#,##0.0"
                points -> "#,##0.##"
                else -> "#,##0.00"
            }
            val s = DecimalFormat(pattern, DecimalFormatSymbols(Locale.US)).apply {
                roundingMode = RoundingMode.HALF_UP
            }.format(bd)
            when (unit) {
                "CNY" -> "¥$s"
                "%" -> "$s%"
                "credits" -> if (points) "$s 分" else "$unit $s"
                else -> if (points) "$s $unit" else "$unit $s"
            }
        } catch (_: Exception) {
            value
        }
    }
}
