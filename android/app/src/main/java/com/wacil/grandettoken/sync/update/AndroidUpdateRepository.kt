package com.wacil.grandettoken.sync.update

import android.content.Context
import android.content.Intent
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.pm.PackageInfoCompat
import androidx.core.content.FileProvider
import com.wacil.grandettoken.sync.BuildConfig
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import okhttp3.Request
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.security.MessageDigest
import java.util.Base64
import java.util.concurrent.TimeUnit

sealed interface UpdateCheckResult {
    data object UpToDate : UpdateCheckResult
    data class Available(val payload: UpdatePayload, val artifact: UpdateArtifact) : UpdateCheckResult
}

class UpdateCheckException(val code: String) : Exception(code)

object AndroidUpdateRepository {
    const val STABLE_MANIFEST_URL =
        "https://github.com/wacilimonster-source/Grandettoken/releases/download/android-stable/android-latest.json"

    /** 测试版可通过 GRANDETTOKEN_ANDROID_UPDATE_MANIFEST_URL 指向本地/预发布清单。 */
    private val manifestUrl: String
        get() = BuildConfig.UPDATE_MANIFEST_URL.ifBlank { STABLE_MANIFEST_URL }
    private val allowTestUrl = BuildConfig.ALLOW_TEST_UPDATE_URL

    private const val KEY_ID = "grandettoken-android-updates-v1"
    private const val MAX_MANIFEST_BYTES = 64 * 1024
    private const val MAX_APK_BYTES = 150L * 1024L * 1024L
    private val trustedHosts = setOf(
        "github.com",
        "release-assets.githubusercontent.com",
        "objects.githubusercontent.com",
    )
    private val client = OkHttpClient.Builder()
        .connectTimeout(15, TimeUnit.SECONDS)
        .readTimeout(30, TimeUnit.SECONDS)
        .callTimeout(45, TimeUnit.SECONDS)
        .followRedirects(true)
        .build()

    suspend fun check(context: Context): UpdateCheckResult = withContext(Dispatchers.IO) {
        val prefs = context.getSharedPreferences("grandettoken_update", Context.MODE_PRIVATE)
        val requestBuilder = Request.Builder().url(manifestUrl).get()
        prefs.getString("manifestEtag", null)?.let { requestBuilder.header("If-None-Match", it) }
        val payload = try {
            client.newCall(requestBuilder.build()).execute().use { response ->
                requireTrustedResponse(
                    response.request.url.isHttps,
                    response.request.url.host,
                )
                when (response.code) {
                    304 -> {
                        val cached = prefs.getString("verifiedManifest", null)
                            ?: throw UpdateCheckException("UPDATE_CACHE_MISSING")
                        verifyManifest(cached.toByteArray(Charsets.UTF_8))
                    }
                    200 -> {
                        val raw = response.body?.byteStream()?.use { readBounded(it, MAX_MANIFEST_BYTES) }
                            ?: throw UpdateCheckException("UPDATE_MANIFEST_EMPTY")
                        val verified = verifyManifest(raw)
                        prefs.edit()
                            .putString("verifiedManifest", raw.toString(Charsets.UTF_8))
                            .putString("manifestEtag", response.header("ETag"))
                            .apply()
                        verified
                    }
                    else -> throw UpdateCheckException("UPDATE_HTTP_${response.code}")
                }
            }
        } catch (e: CancellationException) {
            throw e
        } catch (e: UpdateCheckException) {
            throw e
        } catch (_: Exception) {
            throw UpdateCheckException("UPDATE_NETWORK_ERROR")
        }

        if (payload.minSdk > Build.VERSION.SDK_INT) throw UpdateCheckException("UPDATE_UNSUPPORTED_SDK")
        val currentVersion = currentVersionCode(context)
        if (payload.versionCode <= currentVersion) return@withContext UpdateCheckResult.UpToDate
        val artifact = payload.artifacts.singleOrNull { it.abi == "universal" }
            ?: throw UpdateCheckException("UPDATE_NO_COMPATIBLE_APK")
        UpdateCheckResult.Available(payload, artifact)
    }

    suspend fun downloadAndVerify(
        context: Context,
        payload: UpdatePayload,
        artifact: UpdateArtifact,
        onProgress: (bytesRead: Long, totalBytes: Long) -> Unit = { _, _ -> },
    ): File =
        withContext(Dispatchers.IO) {
            val url = artifact.downloadUrl
            if (allowTestUrl) {
                if (!url.startsWith("http://10.0.2.2:")) {
                    throw UpdateCheckException("UPDATE_DOWNLOAD_URL_INVALID")
                }
            } else if (!url.startsWith("https://github.com/wacilimonster-source/Grandettoken/releases/download/")) {
                throw UpdateCheckException("UPDATE_DOWNLOAD_URL_INVALID")
            }
            val request = Request.Builder().url(url).get().build()
            val dir = File(context.cacheDir, "updates")
            if (!dir.exists() && !dir.mkdirs()) throw UpdateCheckException("UPDATE_STORAGE_ERROR")
            cleanupOldDownloads(dir)
            // 进程中断留下的半包不参与下一次校验；重下时从零开始。
            dir.listFiles()?.filter { it.isFile && it.name.endsWith(".part.apk") }?.forEach { it.delete() }
            val target = File(dir, "Grandettoken-android-${payload.versionName}.apk")
            val partial = File(dir, ".${payload.versionCode}.part.apk")
            try {
                client.newCall(request).execute().use { response ->
                    requireTrustedResponse(response.request.url.isHttps, response.request.url.host)
                    if (!response.isSuccessful) throw UpdateCheckException("UPDATE_DOWNLOAD_HTTP_${response.code}")
                    val body = response.body ?: throw UpdateCheckException("UPDATE_DOWNLOAD_EMPTY")
                    val announcedLength = body.contentLength()
                    if (announcedLength > MAX_APK_BYTES || announcedLength >= 0 && announcedLength != artifact.sizeBytes) {
                        throw UpdateCheckException("UPDATE_APK_SIZE_INVALID")
                    }
                    body.byteStream().use { input ->
                        FileOutputStream(partial).use { output ->
                            val buffer = ByteArray(32 * 1024)
                            var total = 0L
                            while (true) {
                                currentCoroutineContext().ensureActive()
                                val read = input.read(buffer)
                                if (read < 0) break
                                total += read
                                if (total > MAX_APK_BYTES || total > artifact.sizeBytes) {
                                    throw UpdateCheckException("UPDATE_APK_SIZE_INVALID")
                                }
                                output.write(buffer, 0, read)
                                onProgress(total, artifact.sizeBytes)
                            }
                            output.fd.sync()
                            if (total != artifact.sizeBytes) throw UpdateCheckException("UPDATE_APK_SIZE_INVALID")
                        }
                    }
                }
                currentCoroutineContext().ensureActive()
                val actualHash = sha256(partial)
                if (!actualHash.equals(artifact.sha256, ignoreCase = true)) {
                    throw UpdateCheckException("UPDATE_APK_HASH_MISMATCH")
                }
                verifyApk(context, partial, payload)
                if (target.exists() && !target.delete()) throw UpdateCheckException("UPDATE_STORAGE_ERROR")
                if (!partial.renameTo(target)) throw UpdateCheckException("UPDATE_STORAGE_ERROR")
                target
            } catch (e: CancellationException) {
                partial.delete()
                throw e
            } catch (e: UpdateCheckException) {
                partial.delete()
                throw e
            } catch (e: Exception) {
                if (allowTestUrl) {
                    android.util.Log.e("GT_UPDATE_TEST", "download/verify failed", e)
                }
                partial.delete()
                throw UpdateCheckException("UPDATE_DOWNLOAD_FAILED")
            }
        }

    fun currentVersionCode(context: Context): Long {
        val info = context.packageManager.getPackageInfo(context.packageName, 0)
        return PackageInfoCompat.getLongVersionCode(info)
    }

    /** 交给系统安装器前记下目标版本；安装成功后的下一次启动用它核对。 */
    fun markInstalling(context: Context, versionCode: Long, apk: File) {
        context.getSharedPreferences("grandettoken_update", Context.MODE_PRIVATE).edit()
            .putLong("pendingInstallVersionCode", versionCode)
            .putString("pendingInstallApk", apk.absolutePath)
            .apply()
    }

    /**
     * 下次启动核对实际 versionCode。只有真的装上（当前版本 >= 待安装版本）才清理
     * 旧 APK、跳过记录和待安装标记；交给安装器本身不算成功。
     */
    fun confirmInstalled(context: Context): Boolean {
        val prefs = context.getSharedPreferences("grandettoken_update", Context.MODE_PRIVATE)
        val pending = prefs.getLong("pendingInstallVersionCode", 0L)
        if (pending <= 0L) return false
        if (currentVersionCode(context) < pending) return false
        prefs.getString("pendingInstallApk", null)?.let { path ->
            runCatching { File(path).delete() }
        }
        prefs.edit()
            .remove("pendingInstallVersionCode")
            .remove("pendingInstallApk")
            .remove("skippedVersionCode")
            .apply()
        return true
    }

    fun canInstallPackages(context: Context): Boolean =
        Build.VERSION.SDK_INT < 26 || context.packageManager.canRequestPackageInstalls()

    fun requestInstallPermission(context: Context) {
        if (Build.VERSION.SDK_INT >= 26) {
            val intent = Intent(
                Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                Uri.parse("package:${context.packageName}"),
            ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            context.startActivity(intent)
        }
    }

    fun installApk(context: Context, apk: File) {
        val uri = FileProvider.getUriForFile(
            context,
            "${context.packageName}.fileprovider",
            apk,
        )
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        context.startActivity(intent)
    }

    private fun verifyManifest(raw: ByteArray): UpdatePayload {
        val key = runCatching { Base64.getDecoder().decode(BuildConfig.UPDATE_MANIFEST_PUBLIC_KEY) }
            .getOrElse { throw UpdateCheckException("UPDATE_KEY_INVALID") }
        return try {
            SignedUpdateManifestVerifier.verify(raw, key, KEY_ID, allowTestUrl)
        } catch (e: IllegalArgumentException) {
            throw UpdateCheckException(e.message ?: "UPDATE_MANIFEST_INVALID")
        }
    }

    private fun verifyApk(context: Context, apk: File, payload: UpdatePayload) {
        if (BuildConfig.APK_CERT_SHA256.isBlank()) throw UpdateCheckException("UPDATE_CERT_NOT_CONFIGURED")
        val flags = if (Build.VERSION.SDK_INT >= 28) {
            PackageManager.GET_SIGNING_CERTIFICATES
        } else {
            @Suppress("DEPRECATION") PackageManager.GET_SIGNATURES
        }
        val info = context.packageManager.getPackageArchiveInfo(apk.absolutePath, flags)
            ?: throw UpdateCheckException("UPDATE_APK_INVALID")
        if (info.packageName != context.packageName) throw UpdateCheckException("UPDATE_APP_ID_MISMATCH")
        if (PackageInfoCompat.getLongVersionCode(info) != payload.versionCode) {
            throw UpdateCheckException("UPDATE_VERSION_MISMATCH")
        }
        val actualMinSdk = info.applicationInfo?.minSdkVersion ?: 1
        if (actualMinSdk > Build.VERSION.SDK_INT || actualMinSdk != payload.minSdk) {
            throw UpdateCheckException("UPDATE_UNSUPPORTED_SDK")
        }
        @Suppress("DEPRECATION")
        val signatures = if (Build.VERSION.SDK_INT >= 28) {
            info.signingInfo?.apkContentsSigners
        } else {
            info.signatures
        }
        val certificate = signatures?.firstOrNull()?.toByteArray()
            ?: throw UpdateCheckException("UPDATE_APK_CERT_INVALID")
        val fingerprint = MessageDigest.getInstance("SHA-256").digest(certificate).toHex()
        if (!fingerprint.equals(BuildConfig.APK_CERT_SHA256, ignoreCase = true)) {
            throw UpdateCheckException("UPDATE_APK_CERT_MISMATCH")
        }
    }

    private fun requireTrustedResponse(https: Boolean, host: String) {
        if (allowTestUrl && !https && host == "10.0.2.2") return
        if (!https || host.lowercase() !in trustedHosts) throw UpdateCheckException("UPDATE_HOST_NOT_TRUSTED")
    }

    private fun readBounded(input: java.io.InputStream, maxBytes: Int): ByteArray {
        val output = ByteArrayOutputStream()
        val buffer = ByteArray(8192)
        var total = 0
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            total += count
            if (total > maxBytes) throw UpdateCheckException("UPDATE_MANIFEST_SIZE")
            output.write(buffer, 0, count)
        }
        return output.toByteArray()
    }

    private fun sha256(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(32 * 1024)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                digest.update(buffer, 0, count)
            }
        }
        return digest.digest().toHex()
    }

    private fun ByteArray.toHex(): String = joinToString("") { "%02x".format(it) }

    private fun cleanupOldDownloads(dir: File) {
        val cutoff = System.currentTimeMillis() - TimeUnit.DAYS.toMillis(7)
        dir.listFiles()?.filter { it.isFile && it.lastModified() < cutoff }?.forEach { it.delete() }
    }
}
