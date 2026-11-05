package com.wacil.grandettoken.sync

/** 诊断只展示失败数量与规范错误码，绝不展示 source ID、文件名或原始响应。 */
internal data class FailureSummary(val count: Int, val code: String)

internal fun summarizeFailures(failedSources: List<String>): List<FailureSummary> {
    return failedSources
        .map { raw ->
            val code = raw.substringAfter(':', missingDelimiterValue = "")
                .takeIf { it.matches(Regex("^[A-Z][A-Z0-9_]{0,63}$")) }
                ?: "SYNC_ERROR"
            code
        }
        .groupingBy { it }
        .eachCount()
        .map { (code, count) -> FailureSummary(count, code) }
        .sortedWith(compareByDescending<FailureSummary> { it.count }.thenBy { it.code })
}
