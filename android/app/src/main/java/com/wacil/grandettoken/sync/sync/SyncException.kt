package com.wacil.grandettoken.sync.sync

/** 稳定错误码与 Android 端共用的异常类型。消息只做人读补充，不含响应体/密钥。 */
class SyncException(
    val code: String,
    override val message: String,
    val retryAfterSec: Long? = null,
    val resetAt: Long? = null,
) : Exception(message)

object SyncCodes {
    const val INVALID_CODE = "SYNC_CODE_INVALID"
    const val ENVELOPE_INVALID = "SYNC_ENVELOPE_INVALID"
    const val DECRYPT_FAILED = "SYNC_DECRYPT_FAILED"
    const val PAYLOAD_INVALID = "SYNC_PAYLOAD_INVALID"
    const val UNSUPPORTED_VERSION = "SYNC_UNSUPPORTED_VERSION"
    const val TRUNCATED = "SYNC_TRUNCATED"
    const val TOO_LARGE = "SYNC_TOO_LARGE"
    const val RATE_LIMITED = "SYNC_RATE_LIMITED"
    const val AUTH = "SYNC_AUTH"
    const val FORBIDDEN = "SYNC_FORBIDDEN"
    const val NOT_FOUND = "SYNC_NOT_FOUND"
    const val CONFLICT = "SYNC_CONFLICT"
    const val NETWORK = "SYNC_NETWORK"
    const val UPSTREAM = "SYNC_UPSTREAM"
    const val NOT_CONFIGURED = "SYNC_NOT_CONFIGURED"
    const val GIST_ID_INVALID = "SYNC_GIST_ID_INVALID"
    const val SOURCE_INVALID = "SYNC_SOURCE_INVALID"
    const val KEY_INVALID = "SYNC_KEY_INVALID"
    const val EMPTY_GROUP = "SYNC_EMPTY_GROUP"
}
