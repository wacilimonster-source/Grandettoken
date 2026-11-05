package com.wacil.grandettoken.sync

import com.wacil.grandettoken.sync.sync.SyncCode
import com.wacil.grandettoken.sync.sync.SyncException

/** 扫码结果的本地判定：先校验格式与 CRC，再交给加入流程访问网络。 */
internal sealed interface ScanOutcome {
    data class Accepted(val code: String) : ScanOutcome
    data class Rejected(val message: String) : ScanOutcome
}

internal fun interpretPairingScan(text: String?): ScanOutcome {
    val trimmed = text?.trim().orEmpty()
    if (trimmed.isEmpty()) {
        return ScanOutcome.Rejected("已取消扫描,仍可手动输入同步码。")
    }
    return try {
        SyncCode.decode(trimmed)
        ScanOutcome.Accepted(trimmed)
    } catch (e: SyncException) {
        ScanOutcome.Rejected("同步码无效: ${e.message ?: "请重新扫描"}。也可以手动粘贴。")
    }
}
