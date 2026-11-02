package com.wacil.grandettoken.sync.sync

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import org.json.JSONObject
import java.security.KeyStore
import java.security.UnrecoverableKeyException
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Android Keystore 只做“本机包装密钥”：用它加密同步主密钥、OAuth token 与缓存，
 * 密文进应用私有 SharedPreferences。Keystore 本身不直接保存任意字节。
 */
class KeystoreBox {
    private val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    private fun key(): SecretKey {
        (keyStore.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(
            KeyGenParameterSpec.Builder(
                ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build()
        )
        return generator.generateKey()
    }

    fun encrypt(plain: ByteArray): String {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key())
        val iv = cipher.iv
        val ciphertext = cipher.doFinal(plain)
        return Base64.getEncoder().encodeToString(iv + ciphertext)
    }

    fun decrypt(encoded: String): ByteArray {
        try {
            val all = Base64.getDecoder().decode(encoded)
            if (all.size < 12 + 16) {
                throw SyncException(SyncCodes.KEY_INVALID, "本机包装密文损坏")
            }
            val iv = all.copyOfRange(0, 12)
            val ciphertext = all.copyOfRange(12, all.size)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(
                Cipher.DECRYPT_MODE,
                key(),
                GCMParameterSpec(128, iv),
            )
            return cipher.doFinal(ciphertext)
        } catch (e: KeyPermanentlyInvalidatedException) {
            throw SyncException(SyncCodes.KEY_INVALID, "本机密钥已失效，请重新配对")
        } catch (e: UnrecoverableKeyException) {
            throw SyncException(SyncCodes.KEY_INVALID, "本机密钥不可用，请重新配对")
        } catch (e: SyncException) {
            throw e
        } catch (_: Exception) {
            throw SyncException(SyncCodes.KEY_INVALID, "本机加密数据不可读，请重新配对")
        }
    }

    companion object {
        private const val ALIAS = "grandettoken-sync-box"
    }
}

data class GroupInfo(
    val relayId: String,
    val keyEpoch: Int,
    val hasToken: Boolean,
    val lastRefresh: Long,
    val backgroundMinutes: Int,
)

class SyncStore(context: Context) {
    private val prefs = context.applicationContext
        .getSharedPreferences("grandettoken_sync", Context.MODE_PRIVATE)
    private val box = KeystoreBox()

    fun hasGroup(): Boolean = prefs.contains(KEY_GROUP)

    fun saveGroup(code: SyncCode) {
        val payload = JSONObject()
            .put("relayId", SyncCrypto.hexEncode(code.relayId))
            .put("key", SyncCrypto.hexEncode(code.key))
            .put("keyEpoch", code.keyEpoch)
            .toString()
        prefs.edit().putString(KEY_GROUP, box.encrypt(payload.toByteArray())).apply()
    }

    fun loadGroup(): SyncCode? {
        val encrypted = prefs.getString(KEY_GROUP, null) ?: return null
        return try {
            val json = JSONObject(String(box.decrypt(encrypted), Charsets.UTF_8))
            val relay = SyncCrypto.hexDecode(json.getString("relayId")) ?: return null
            val key = SyncCrypto.hexDecode(json.getString("key")) ?: return null
            if (relay.size != SyncCrypto.RELAY_ID_LEN || key.size != SyncCrypto.KEY_LEN) return null
            SyncCode(relay, key, json.optInt("keyEpoch", 0))
        } catch (_: Exception) {
            null
        }
    }

    fun clearGroup() {
        prefs.edit()
            .remove(KEY_GROUP)
            .remove(KEY_CACHE)
            .remove(KEY_ETAG)
            .remove(KEY_LAST_REFRESH)
            .remove(KEY_WATERMARKS)
            .remove(KEY_NEXT_ALLOWED)
            .apply()
    }

    fun saveCache(merged: MergedSnapshot) {
        val text = SyncJson.encodeToString(MergedSnapshot.serializer(), merged)
        prefs.edit().putString(KEY_CACHE, box.encrypt(text.toByteArray())).apply()
    }

    fun loadCache(): MergedSnapshot? {
        val encrypted = prefs.getString(KEY_CACHE, null) ?: return null
        return try {
            SyncJson.decodeFromString(
                MergedSnapshot.serializer(),
                String(box.decrypt(encrypted), Charsets.UTF_8),
            )
        } catch (_: Exception) {
            null
        }
    }

    fun clearCache() {
        prefs.edit()
            .remove(KEY_CACHE)
            .remove(KEY_ETAG)
            .remove(KEY_WATERMARKS)
            .apply()
    }

    fun saveEtag(etag: String?) {
        prefs.edit().apply {
            if (etag.isNullOrBlank()) remove(KEY_ETAG) else putString(KEY_ETAG, etag)
        }.apply()
    }

    fun loadEtag(): String? = prefs.getString(KEY_ETAG, null)

    fun saveLastRefresh(ts: Long) {
        prefs.edit().putLong(KEY_LAST_REFRESH, ts).apply()
    }

    fun loadLastRefresh(): Long = prefs.getLong(KEY_LAST_REFRESH, 0L)

    fun saveBackgroundMinutes(minutes: Int) {
        prefs.edit().putInt(KEY_BACKGROUND, minutes.coerceIn(15, 60)).apply()
    }

    fun loadBackgroundMinutes(): Int = prefs.getInt(KEY_BACKGROUND, 15)

    fun saveToken(token: String) {
        prefs.edit().putString(KEY_TOKEN, box.encrypt(token.toByteArray())).apply()
    }

    fun loadToken(): String? {
        val encrypted = prefs.getString(KEY_TOKEN, null) ?: return null
        return try {
            String(box.decrypt(encrypted), Charsets.UTF_8)
        } catch (_: Exception) {
            null
        }
    }

    fun clearToken() {
        prefs.edit().remove(KEY_TOKEN).apply()
    }

    fun hasToken(): Boolean = prefs.contains(KEY_TOKEN)

    fun saveWatermark(source: String, seq: Long, hash: String) {
        val map = loadWatermarks()
        map[source] = "$seq:$hash"
        prefs.edit().putString(KEY_WATERMARKS, JSONObject(map).toString()).apply()
    }

    fun loadWatermark(source: String): Pair<Long, String>? {
        val raw = loadWatermarks()[source] ?: return null
        val parts = raw.split(':', limit = 2)
        if (parts.size != 2) return null
        val seq = parts[0].toLongOrNull() ?: return null
        return seq to parts[1]
    }

    private fun loadWatermarks(): MutableMap<String, String> {
        val raw = prefs.getString(KEY_WATERMARKS, null) ?: return mutableMapOf()
        return try {
            val obj = JSONObject(raw)
            val out = mutableMapOf<String, String>()
            for (key in obj.keys()) out[key] = obj.getString(key)
            out
        } catch (_: Exception) {
            mutableMapOf()
        }
    }

    fun saveNextAllowed(ts: Long) {
        prefs.edit().putLong(KEY_NEXT_ALLOWED, ts).apply()
    }

    fun loadNextAllowed(): Long = prefs.getLong(KEY_NEXT_ALLOWED, 0L)

    fun groupInfo(): GroupInfo? {
        val group = loadGroup() ?: return null
        return GroupInfo(
            relayId = group.gistIdHex(),
            keyEpoch = group.keyEpoch,
            hasToken = hasToken(),
            lastRefresh = loadLastRefresh(),
            backgroundMinutes = loadBackgroundMinutes(),
        )
    }

    companion object {
        private const val KEY_GROUP = "group"
        private const val KEY_CACHE = "cache"
        private const val KEY_ETAG = "etag"
        private const val KEY_LAST_REFRESH = "lastRefresh"
        private const val KEY_BACKGROUND = "backgroundMinutes"
        private const val KEY_TOKEN = "token"
        private const val KEY_WATERMARKS = "watermarks"
        private const val KEY_NEXT_ALLOWED = "nextAllowed"
    }
}
