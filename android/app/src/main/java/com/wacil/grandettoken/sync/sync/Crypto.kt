package com.wacil.grandettoken.sync.sync

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.ByteArrayOutputStream
import java.security.MessageDigest
import java.security.SecureRandom
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

@Serializable
data class Envelope(val v: Int, val n: String, val c: String)

object SyncCrypto {
    const val ENVELOPE_VERSION = 1
    const val KEY_LEN = 32
    const val RELAY_ID_LEN = 16
    const val NONCE_LEN = 12
    const val TAG_LEN = 16

    private val json = Json { ignoreUnknownKeys = true }
    private val rng = SecureRandom()
    private val saltInput = "grandettoken-sync-v1/salt".toByteArray(Charsets.UTF_8)
    private val infoPrefix = "grandettoken-sync-v1/source-key"

    fun randomBytes(size: Int): ByteArray = ByteArray(size).also { rng.nextBytes(it) }

    fun sha256(data: ByteArray): ByteArray =
        MessageDigest.getInstance("SHA-256").digest(data)

    private fun hmac(key: ByteArray, vararg parts: ByteArray): ByteArray {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(key, "HmacSHA256"))
        parts.forEach { mac.update(it) }
        return mac.doFinal()
    }

    /** RFC 5869 HKDF-SHA256。 */
    fun hkdf(ikm: ByteArray, salt: ByteArray, info: ByteArray, outLen: Int): ByteArray {
        val prk = hmac(salt, ikm)
        val out = ByteArrayOutputStream()
        var t = ByteArray(0)
        var counter = 1
        while (out.size() < outLen) {
            t = hmac(prk, t, info, byteArrayOf(counter.toByte()))
            out.write(t)
            counter += 1
        }
        return out.toByteArray().copyOf(outLen)
    }

    fun hkdfSalt(): ByteArray = sha256(saltInput)

    fun validSource(source: String): Boolean =
        Regex("^s_[0-9a-fA-F]{32}$").matches(source)

    fun normalizeSource(source: String): String? {
        val s = source.trim().lowercase()
        return if (validSource(s)) s else null
    }

    fun deriveSourceKey(
        master: ByteArray,
        relayId: ByteArray,
        source: String,
        keyEpoch: Int,
    ): ByteArray {
        require(master.size == KEY_LEN) { "master key must be 32 bytes" }
        require(relayId.size == RELAY_ID_LEN) { "relay id must be 16 bytes" }
        val normalized = normalizeSource(source)
            ?: throw SyncException(SyncCodes.SOURCE_INVALID, "写入者 ID 格式不正确")
        val info = "$infoPrefix|${hexEncode(relayId)}|$normalized|$keyEpoch"
            .toByteArray(Charsets.UTF_8)
        return hkdf(master, hkdfSalt(), info, KEY_LEN)
    }

    fun aadBytes(source: String, keyEpoch: Int): ByteArray =
        "v1|$source|$keyEpoch".toByteArray(Charsets.UTF_8)

    fun seal(
        master: ByteArray,
        relayId: ByteArray,
        source: String,
        keyEpoch: Int,
        plaintext: ByteArray,
        nonce: ByteArray = randomBytes(NONCE_LEN),
    ): ByteArray {
        if (nonce.size != NONCE_LEN) {
            throw SyncException(SyncCodes.ENVELOPE_INVALID, "nonce 长度不是 12 字节")
        }
        val key = deriveSourceKey(master, relayId, source, keyEpoch)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.ENCRYPT_MODE,
            SecretKeySpec(key, "AES"),
            GCMParameterSpec(TAG_LEN * 8, nonce),
        )
        cipher.updateAAD(aadBytes(source, keyEpoch))
        val ciphertext = cipher.doFinal(plaintext)
        val envelope = Envelope(
            v = ENVELOPE_VERSION,
            n = base64UrlEncode(nonce),
            c = base64UrlEncode(ciphertext),
        )
        return json.encodeToString(Envelope.serializer(), envelope).toByteArray(Charsets.UTF_8)
    }

    fun open(
        master: ByteArray,
        relayId: ByteArray,
        source: String,
        keyEpoch: Int,
        envelopeBytes: ByteArray,
    ): ByteArray {
        val envelope = try {
            json.decodeFromString(
                Envelope.serializer(),
                envelopeBytes.toString(Charsets.UTF_8),
            )
        } catch (_: Exception) {
            throw SyncException(SyncCodes.ENVELOPE_INVALID, "密文外壳不是合法 JSON")
        }
        if (envelope.v != ENVELOPE_VERSION) {
            throw SyncException(
                SyncCodes.UNSUPPORTED_VERSION,
                "不支持的密文外壳版本: ${envelope.v}",
            )
        }
        val nonce = try {
            base64UrlDecode(envelope.n)
        } catch (_: Exception) {
            throw SyncException(SyncCodes.ENVELOPE_INVALID, "nonce 不是合法 base64url")
        }
        if (nonce.size != NONCE_LEN) {
            throw SyncException(SyncCodes.ENVELOPE_INVALID, "nonce 长度不是 12 字节")
        }
        val ciphertext = try {
            base64UrlDecode(envelope.c)
        } catch (_: Exception) {
            throw SyncException(SyncCodes.ENVELOPE_INVALID, "密文不是合法 base64url")
        }
        if (ciphertext.size < TAG_LEN) {
            throw SyncException(SyncCodes.ENVELOPE_INVALID, "密文短于 GCM tag")
        }
        val key = deriveSourceKey(master, relayId, source, keyEpoch)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(
            Cipher.DECRYPT_MODE,
            SecretKeySpec(key, "AES"),
            GCMParameterSpec(TAG_LEN * 8, nonce),
        )
        cipher.updateAAD(aadBytes(source, keyEpoch))
        return try {
            cipher.doFinal(ciphertext)
        } catch (_: Exception) {
            throw SyncException(
                SyncCodes.DECRYPT_FAILED,
                "密文校验失败（密钥、AAD 或内容被篡改）",
            )
        }
    }

    fun base64UrlEncode(bytes: ByteArray): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    fun base64UrlDecode(text: String): ByteArray =
        Base64.getUrlDecoder().decode(text)

    fun hexEncode(bytes: ByteArray): String {
        val sb = StringBuilder(bytes.size * 2)
        for (b in bytes) {
            val v = b.toInt() and 0xFF
            sb.append("0123456789abcdef"[v ushr 4])
            sb.append("0123456789abcdef"[v and 0x0F])
        }
        return sb.toString()
    }

    fun hexDecode(text: String): ByteArray? {
        if (text.length % 2 != 0) return null
        val out = ByteArray(text.length / 2)
        for (i in out.indices) {
            val hi = hexVal(text[i * 2]) ?: return null
            val lo = hexVal(text[i * 2 + 1]) ?: return null
            out[i] = ((hi shl 4) or lo).toByte()
        }
        return out
    }

    private fun hexVal(c: Char): Int? = when (c) {
        in '0'..'9' -> c - '0'
        in 'a'..'f' -> c - 'a' + 10
        in 'A'..'F' -> c - 'A' + 10
        else -> null
    }
}
