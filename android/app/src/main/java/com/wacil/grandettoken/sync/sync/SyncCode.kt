package com.wacil.grandettoken.sync.sync

import java.io.ByteArrayOutputStream
import java.util.zip.CRC32

/**
 * `GT1.<base64url(payload||crc32)>`，payload 固定 51 字节：
 * version(1)|relayKind(1)|keyEpoch(1)|relayId(16)|key(32)。
 */
data class SyncCode(
    val relayId: ByteArray,
    val key: ByteArray,
    val keyEpoch: Int,
) {
    fun gistIdHex(): String = SyncCrypto.hexEncode(relayId)

    fun encode(): String {
        require(relayId.size == SyncCrypto.RELAY_ID_LEN)
        require(key.size == SyncCrypto.KEY_LEN)
        val payload = ByteArrayOutputStream().apply {
            write(VERSION)
            write(RELAY_KIND_GIST)
            write(keyEpoch and 0xFF)
            write(relayId)
            write(key)
        }.toByteArray()
        val crc = CRC32().apply { update(payload) }.value
        val all = payload + byteArrayOf(
            ((crc ushr 24) and 0xFF).toByte(),
            ((crc ushr 16) and 0xFF).toByte(),
            ((crc ushr 8) and 0xFF).toByte(),
            (crc and 0xFF).toByte(),
        )
        return "$MAGIC.${SyncCrypto.base64UrlEncode(all)}"
    }

    override fun equals(other: Any?): Boolean =
        other is SyncCode &&
            relayId.contentEquals(other.relayId) &&
            key.contentEquals(other.key) &&
            keyEpoch == other.keyEpoch

    override fun hashCode(): Int =
        31 * (31 * relayId.contentHashCode() + key.contentHashCode()) + keyEpoch

    companion object {
        const val MAGIC = "GT1"
        const val VERSION = 1
        const val RELAY_KIND_GIST = 1
        const val PAYLOAD_LEN = 1 + 1 + 1 + 16 + 32
        const val CODED_LEN = PAYLOAD_LEN + 4

        fun decode(text: String): SyncCode {
            val trimmed = text.trim()
            if (!trimmed.startsWith("$MAGIC.")) {
                throw SyncException(SyncCodes.INVALID_CODE, "同步码缺少 GT1. 前缀")
            }
            val bytes = try {
                SyncCrypto.base64UrlDecode(trimmed.substring("$MAGIC.".length))
            } catch (_: Exception) {
                throw SyncException(SyncCodes.INVALID_CODE, "同步码不是合法 base64url")
            }
            if (bytes.size != CODED_LEN) {
                throw SyncException(SyncCodes.INVALID_CODE, "同步码长度不正确(${bytes.size})")
            }
            val payload = bytes.copyOfRange(0, PAYLOAD_LEN)
            val expected = CRC32().apply { update(payload) }.value
            val got = ((bytes[PAYLOAD_LEN].toLong() and 0xFF) shl 24) or
                ((bytes[PAYLOAD_LEN + 1].toLong() and 0xFF) shl 16) or
                ((bytes[PAYLOAD_LEN + 2].toLong() and 0xFF) shl 8) or
                (bytes[PAYLOAD_LEN + 3].toLong() and 0xFF)
            if (expected != got) {
                throw SyncException(SyncCodes.INVALID_CODE, "同步码校验失败")
            }
            val version = payload[0].toInt() and 0xFF
            if (version != VERSION) {
                throw SyncException(
                    SyncCodes.UNSUPPORTED_VERSION,
                    "不支持的同步码版本: $version",
                )
            }
            val relayKind = payload[1].toInt() and 0xFF
            if (relayKind != RELAY_KIND_GIST) {
                throw SyncException(SyncCodes.INVALID_CODE, "未知的中继类型")
            }
            val keyEpoch = payload[2].toInt() and 0xFF
            val relayId = payload.copyOfRange(3, 3 + 16)
            val key = payload.copyOfRange(3 + 16, PAYLOAD_LEN)
            return SyncCode(relayId, key, keyEpoch)
        }

        fun parseGistId(hex: String): ByteArray {
            if (hex.length != 32) {
                throw SyncException(SyncCodes.GIST_ID_INVALID, "Gist ID 必须是 32 位十六进制")
            }
            return SyncCrypto.hexDecode(hex)
                ?: throw SyncException(SyncCodes.GIST_ID_INVALID, "Gist ID 含非十六进制字符")
        }

        fun validGistId(hex: String): Boolean =
            hex.length == 32 && hex.all { it.isDigit() || it.lowercaseChar() in 'a'..'f' }

        fun fileNameForSource(source: String): String {
            val normalized = SyncCrypto.normalizeSource(source)
                ?: throw SyncException(SyncCodes.SOURCE_INVALID, "写入者 ID 格式不正确")
            return "gt_$normalized.json"
        }

        fun sourceFromFileName(name: String): String? {
            if (!name.startsWith("gt_") || !name.endsWith(".json")) return null
            return SyncCrypto.normalizeSource(name.removePrefix("gt_").removeSuffix(".json"))
        }
    }
}
