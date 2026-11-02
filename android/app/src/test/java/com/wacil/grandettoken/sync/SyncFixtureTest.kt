package com.wacil.grandettoken.sync

import com.wacil.grandettoken.sync.sync.Envelope
import com.wacil.grandettoken.sync.sync.SyncCode
import com.wacil.grandettoken.sync.sync.SyncCodes
import com.wacil.grandettoken.sync.sync.SyncCrypto
import com.wacil.grandettoken.sync.sync.SyncException
import com.wacil.grandettoken.sync.sync.SyncJson
import com.wacil.grandettoken.sync.sync.SyncSnapshot
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNotNull
import kotlin.test.assertTrue

/**
 * 与 Rust `app/core/tests/sync_fixtures.rs` 读同一份 contract/fixtures/sync-v1.json。
 * 任何一处 HKDF/AAD/base64url/GCM tag/同步码编码不一致，这里都会红。
 */
class SyncFixtureTest {
    private val fixture: JsonObject by lazy {
        val stream = javaClass.classLoader.getResourceAsStream("sync-v1.json")
        assertNotNull(stream, "缺少 sync-v1.json（Gradle 会从 contract/fixtures 复制）")
        Json.parseToJsonElement(stream.bufferedReader().readText()).jsonObject
    }

    private fun hex(key: String): ByteArray {
        val text = fixture[key]!!.jsonPrimitive.content
        return assertNotNull(SyncCrypto.hexDecode(text), "$key 不是合法十六进制")
    }

    @Test
    fun derived_key_and_aad_match_fixture() {
        val master = hex("masterKeyHex")
        val relay = hex("relayIdHex")
        val source = fixture["source"]!!.jsonPrimitive.content
        val epoch = fixture["keyEpoch"]!!.jsonPrimitive.content.toInt()
        val derived = SyncCrypto.deriveSourceKey(master, relay, source, epoch)
        assertEquals(fixture["derivedKeyHex"]!!.jsonPrimitive.content, SyncCrypto.hexEncode(derived))
        assertEquals(fixture["aadHex"]!!.jsonPrimitive.content, SyncCrypto.hexEncode(SyncCrypto.aadBytes(source, epoch)))
    }

    @Test
    fun envelope_decrypts_to_plaintext() {
        val master = hex("masterKeyHex")
        val relay = hex("relayIdHex")
        val source = fixture["source"]!!.jsonPrimitive.content
        val epoch = fixture["keyEpoch"]!!.jsonPrimitive.content.toInt()
        val envelope = fixture["envelope"]!!.toString().toByteArray()
        val plaintext = SyncCrypto.open(master, relay, source, epoch, envelope)
        assertEquals(fixture["plaintextJson"]!!.jsonPrimitive.content, plaintext.toString(Charsets.UTF_8))
        val snap = SyncJson.decodeFromString(SyncSnapshot.serializer(), plaintext.toString(Charsets.UTF_8))
        assertTrue(com.wacil.grandettoken.sync.sync.SyncMerge.validate(snap))
        assertEquals(7, snap.seq)
        assertEquals(3, snap.channels.size)
    }

    @Test
    fun envelope_is_reproducible() {
        val master = hex("masterKeyHex")
        val relay = hex("relayIdHex")
        val nonce = hex("nonceHex")
        val source = fixture["source"]!!.jsonPrimitive.content
        val plaintext = fixture["plaintextJson"]!!.jsonPrimitive.content.toByteArray()
        val sealed = SyncCrypto.seal(master, relay, source, 0, plaintext, nonce)
        val got = Json.parseToJsonElement(sealed.toString(Charsets.UTF_8))
        assertEquals(fixture["envelope"], got)
    }

    @Test
    fun sync_code_roundtrips() {
        val master = hex("masterKeyHex")
        val relay = hex("relayIdHex")
        val text = fixture["syncCode"]!!.jsonPrimitive.content
        val code = SyncCode.decode(text)
        assertEquals(master.toList(), code.key.toList())
        assertEquals(relay.toList(), code.relayId.toList())
        assertEquals(0, code.keyEpoch)
        assertEquals(text, code.encode())
    }

    @Test
    fun rejects_wrong_key_aad_tamper_truncation_and_typo() {
        val master = hex("masterKeyHex")
        val relay = hex("relayIdHex")
        val source = fixture["source"]!!.jsonPrimitive.content
        val envelope = fixture["envelope"]!!.toString().toByteArray()

        val wrongKey = master.copyOf().also { it[0] = (it[0].toInt() xor 1).toByte() }
        assertEquals(
            SyncCodes.DECRYPT_FAILED,
            assertFailsWith<SyncException> {
                SyncCrypto.open(wrongKey, relay, source, 0, envelope)
            }.code,
        )
        assertEquals(
            SyncCodes.DECRYPT_FAILED,
            assertFailsWith<SyncException> {
                SyncCrypto.open(master, relay, "s_0123456789abcdef0123456789abcdee", 0, envelope)
            }.code,
        )

        val env = SyncJson.decodeFromString(Envelope.serializer(), envelope.toString(Charsets.UTF_8))
        val ct = SyncCrypto.base64UrlDecode(env.c)
        ct[0] = (ct[0].toInt() xor 1).toByte()
        val tampered = SyncJson.encodeToString(
            Envelope.serializer(),
            env.copy(c = SyncCrypto.base64UrlEncode(ct)),
        ).toByteArray()
        assertEquals(
            SyncCodes.DECRYPT_FAILED,
            assertFailsWith<SyncException> {
                SyncCrypto.open(master, relay, source, 0, tampered)
            }.code,
        )

        val truncated = SyncJson.encodeToString(
            Envelope.serializer(),
            env.copy(c = SyncCrypto.base64UrlEncode(ByteArray(4))),
        ).toByteArray()
        assertEquals(
            SyncCodes.ENVELOPE_INVALID,
            assertFailsWith<SyncException> {
                SyncCrypto.open(master, relay, source, 0, truncated)
            }.code,
        )

        val text = fixture["syncCode"]!!.jsonPrimitive.content
        val chars = text.toCharArray()
        val idx = 10 // 不要在最后一个 base64 字符上翻转：那几位可能只是填充位
        chars[idx] = if (chars[idx] == 'A') 'B' else 'A'
        val broken = String(chars)
        assertEquals(
            SyncCodes.INVALID_CODE,
            assertFailsWith<SyncException> { SyncCode.decode(broken) }.code,
        )
    }
}
