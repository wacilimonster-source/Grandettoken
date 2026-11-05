package com.wacil.grandettoken.sync.update

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import org.bouncycastle.crypto.params.Ed25519PublicKeyParameters
import org.bouncycastle.crypto.signers.Ed25519Signer
import java.nio.charset.StandardCharsets
import java.util.Base64

@Serializable
data class SignedUpdateManifest(
    val keyId: String,
    val payload: String,
    val signature: String,
)

@Serializable
data class UpdatePayload(
    val schemaVersion: Int,
    val channel: String,
    val applicationId: String,
    val versionName: String,
    val versionCode: Long,
    val publishedAt: String,
    val notes: String,
    val releasePage: String,
    val minSdk: Int,
    val artifacts: List<UpdateArtifact>,
)

@Serializable
data class UpdateArtifact(
    val abi: String,
    val sizeBytes: Long,
    val sha256: String,
    val downloadUrl: String,
)

object SignedUpdateManifestVerifier {
    private const val MAX_MANIFEST_BYTES = 64 * 1024
    private const val MAX_PAYLOAD_BYTES = 48 * 1024
    private const val MAX_APK_BYTES = 150L * 1024L * 1024L
    private const val REPOSITORY = "wacilimonster-source/Grandettoken"
    private const val APPLICATION_ID = "com.wacil.grandettoken.sync"
    private val json = Json { ignoreUnknownKeys = false }

    fun verify(
        rawManifest: ByteArray,
        publicKey: ByteArray,
        expectedKeyId: String,
        allowTestUrls: Boolean = false,
    ): UpdatePayload {
        require(rawManifest.isNotEmpty() && rawManifest.size <= MAX_MANIFEST_BYTES) {
            "UPDATE_MANIFEST_SIZE"
        }
        require(publicKey.size == Ed25519PublicKeyParameters.KEY_SIZE) { "UPDATE_KEY_INVALID" }
        val envelope = runCatching {
            json.decodeFromString<SignedUpdateManifest>(
                StandardCharsets.UTF_8.newDecoder().decode(java.nio.ByteBuffer.wrap(rawManifest)).toString()
            )
        }.getOrElse { throw IllegalArgumentException("UPDATE_MANIFEST_INVALID") }
        require(envelope.keyId == expectedKeyId) { "UPDATE_KEY_UNKNOWN" }

        val payloadBytes = runCatching { Base64.getUrlDecoder().decode(envelope.payload) }
            .getOrElse { throw IllegalArgumentException("UPDATE_PAYLOAD_INVALID") }
        val signature = runCatching { Base64.getUrlDecoder().decode(envelope.signature) }
            .getOrElse { throw IllegalArgumentException("UPDATE_SIGNATURE_INVALID") }
        require(payloadBytes.isNotEmpty() && payloadBytes.size <= MAX_PAYLOAD_BYTES) {
            "UPDATE_PAYLOAD_SIZE"
        }
        require(signature.size == 64) { "UPDATE_SIGNATURE_INVALID" }

        val verifier = Ed25519Signer().apply {
            init(false, Ed25519PublicKeyParameters(publicKey, 0))
            update(payloadBytes, 0, payloadBytes.size)
        }
        require(verifier.verifySignature(signature)) { "UPDATE_SIGNATURE_INVALID" }

        val payload = runCatching {
            json.decodeFromString<UpdatePayload>(
                StandardCharsets.UTF_8.newDecoder().decode(java.nio.ByteBuffer.wrap(payloadBytes)).toString()
            )
        }.getOrElse { throw IllegalArgumentException("UPDATE_PAYLOAD_INVALID") }
        validate(payload, allowTestUrls)
        return payload
    }

    private fun validate(payload: UpdatePayload, allowTestUrls: Boolean) {
        require(payload.schemaVersion == 1) { "UPDATE_SCHEMA_UNSUPPORTED" }
        require(payload.channel == "stable") { "UPDATE_CHANNEL_INVALID" }
        require(payload.applicationId == APPLICATION_ID) { "UPDATE_APP_ID_MISMATCH" }
        require(payload.versionName.matches(Regex("[0-9]+\\.[0-9]+\\.[0-9]+"))) {
            "UPDATE_VERSION_INVALID"
        }
        require(payload.versionCode > 0) { "UPDATE_VERSION_INVALID" }
        require(runCatching { java.time.Instant.parse(payload.publishedAt) }.isSuccess) {
            "UPDATE_DATE_INVALID"
        }
        require(payload.notes.length <= 4000) { "UPDATE_NOTES_TOO_LONG" }
        val expectedTag = "android-v${payload.versionName}"
        if (allowTestUrls) {
            require(payload.releasePage.startsWith("http://10.0.2.2:")) {
                "UPDATE_RELEASE_URL_INVALID"
            }
        } else {
            require(payload.releasePage == "https://github.com/$REPOSITORY/releases/tag/$expectedTag") {
                "UPDATE_RELEASE_URL_INVALID"
            }
        }
        require(payload.minSdk in 21..100) { "UPDATE_MIN_SDK_INVALID" }
        require(payload.artifacts.size == 1) { "UPDATE_ARTIFACT_INVALID" }
        val artifact = payload.artifacts.single()
        require(artifact.abi == "universal") { "UPDATE_ABI_UNSUPPORTED" }
        require(artifact.sizeBytes in 1..MAX_APK_BYTES) { "UPDATE_APK_SIZE_INVALID" }
        require(artifact.sha256.matches(Regex("[a-f0-9]{64}"))) { "UPDATE_HASH_INVALID" }
        if (allowTestUrls) {
            require(artifact.downloadUrl.startsWith("http://10.0.2.2:")) {
                "UPDATE_DOWNLOAD_URL_INVALID"
            }
        } else {
            val expectedUrl = "https://github.com/$REPOSITORY/releases/download/$expectedTag/Grandettoken-$expectedTag.apk"
            require(artifact.downloadUrl == expectedUrl) { "UPDATE_DOWNLOAD_URL_INVALID" }
        }
    }
}
