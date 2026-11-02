package com.wacil.grandettoken.sync.update

import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import org.bouncycastle.crypto.AsymmetricCipherKeyPair
import org.bouncycastle.crypto.generators.Ed25519KeyPairGenerator
import org.bouncycastle.crypto.params.Ed25519KeyGenerationParameters
import org.bouncycastle.crypto.params.Ed25519PrivateKeyParameters
import org.bouncycastle.crypto.params.Ed25519PublicKeyParameters
import org.bouncycastle.crypto.signers.Ed25519Signer
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import java.nio.charset.StandardCharsets
import java.util.Base64
import java.security.SecureRandom

class SignedUpdateManifestVerifierTest {
    private val keyPair: AsymmetricCipherKeyPair = Ed25519KeyPairGenerator().apply {
        init(Ed25519KeyGenerationParameters(SecureRandom()))
    }.generateKeyPair()
    private val privateKey = keyPair.private as Ed25519PrivateKeyParameters
    private val publicKey = (keyPair.public as Ed25519PublicKeyParameters).encoded

    @Test
    fun `verifies raw signed payload before parsing and accepts stable official artifact`() {
        val payload = validPayload().toByteArray(StandardCharsets.UTF_8)
        val result = SignedUpdateManifestVerifier.verify(signedEnvelope(payload), publicKey, "test-key")

        assertEquals(1, result.schemaVersion)
        assertEquals("stable", result.channel)
        assertEquals(1L, result.versionCode)
        assertEquals("universal", result.artifacts.single().abi)
    }

    @Test
    fun `rejects payload changed after signing`() {
        val payload = validPayload().toByteArray(StandardCharsets.UTF_8)
        val signed = Json.decodeFromString<SignedUpdateManifest>(signedEnvelope(payload).toString(StandardCharsets.UTF_8))
        val replacement = if (signed.signature.first() == 'A') 'B' else 'A'
        val changed = Json.encodeToString(signed.copy(signature = replacement + signed.signature.drop(1)))

        val error = assertThrows(IllegalArgumentException::class.java) {
            SignedUpdateManifestVerifier.verify(changed.toByteArray(StandardCharsets.UTF_8), publicKey, "test-key")
        }
        assertEquals("UPDATE_SIGNATURE_INVALID", error.message)
    }

    @Test
    fun `rejects artifact URL outside the immutable official release tag`() {
        val payload = validPayload().replace(
            "https://github.com/wacilimonster-source/Grandettoken/releases/download/android-v0.1.0/Grandettoken-android-v0.1.0.apk",
            "https://example.com/Grandettoken.apk",
        ).toByteArray(StandardCharsets.UTF_8)

        val error = assertThrows(IllegalArgumentException::class.java) {
            SignedUpdateManifestVerifier.verify(signedEnvelope(payload), publicKey, "test-key")
        }
        assertEquals("UPDATE_DOWNLOAD_URL_INVALID", error.message)
    }

    private fun signedEnvelope(payload: ByteArray): ByteArray {
        val signer = Ed25519Signer().apply {
            init(true, privateKey)
            update(payload, 0, payload.size)
        }
        val envelope = SignedUpdateManifest(
            keyId = "test-key",
            payload = Base64.getUrlEncoder().withoutPadding().encodeToString(payload),
            signature = Base64.getUrlEncoder().withoutPadding().encodeToString(signer.generateSignature()),
        )
        return Json.encodeToString(envelope).toByteArray(StandardCharsets.UTF_8)
    }

    private fun validPayload(): String = """
        {
          "schemaVersion":1,
          "channel":"stable",
          "applicationId":"com.wacil.grandettoken.sync",
          "versionName":"0.1.0",
          "versionCode":1,
          "publishedAt":"2026-11-02T00:00:00Z",
          "notes":"release notes",
          "releasePage":"https://github.com/wacilimonster-source/Grandettoken/releases/tag/android-v0.1.0",
          "minSdk":26,
          "artifacts":[{
            "abi":"universal",
            "sizeBytes":1234,
            "sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "downloadUrl":"https://github.com/wacilimonster-source/Grandettoken/releases/download/android-v0.1.0/Grandettoken-android-v0.1.0.apk"
          }]
        }
    """.trimIndent()
}
